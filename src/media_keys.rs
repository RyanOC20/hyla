//! macOS media-key and Now Playing integration.
//!
//! The keyboard's play/pause key (F8 by default) never reaches egui/winit: the
//! OS captures it and delivers a `MPRemoteCommandCenter` command to whichever
//! app currently owns the system "Now Playing" slot. Registering handlers there
//! is the sanctioned way for an audio app to receive the key, and it inherently
//! respects the user's "Use F-keys as standard function keys" setting: the OS
//! only emits the media command when the user actually invokes the media key
//! (plain F8 when media keys are primary, Fn+F8 when function keys are primary).
//!
//! Commands arrive on the main run loop, which keeps running even when the
//! window is hidden or minimized. The handlers therefore act on the shared
//! playback state *directly* rather than forwarding an intent for the egui
//! frame to apply: egui's `ui()` is not called while the window is hidden, so a
//! deferred command would sit unapplied until the window was shown again. hyla
//! also publishes minimal Now Playing info (filename, duration, elapsed, rate)
//! so the OS routes media keys to it and Control Center shows the current track.

/// A play/pause intent from a system media command, resolved against the
/// current pause state when the handler fires.
#[derive(Clone, Copy, Debug)]
enum MediaCommand {
    Play,
    Pause,
    Toggle,
}

pub use imp::MediaControls;

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSCopying, NSData, NSMutableDictionary, NSNumber, NSSize, NSString};
    use objc2_media_player::{
        MPMediaItemArtwork, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
        MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
        MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate,
        MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
        MPRemoteCommandHandlerStatus,
    };

    use super::MediaCommand;
    use crate::audio::shared::SharedState;

    /// Artwork shown in Control Center's Now Playing tile. hyla has no per-track
    /// cover art (a non-goal), so this stands in for every track. It is a
    /// full-bleed variant of the app icon: the dark background reaches every
    /// edge, unlike `icon.png`, whose transparent margins render as a white
    /// border against Control Center's backing.
    const ARTWORK_PNG: &[u8] = include_bytes!("../assets/now-playing.png");

    /// Metadata about the current track, kept so a handler firing while the
    /// window is hidden can rebuild the Now Playing dictionary (which the OS
    /// needs re-set in full to reflect a new elapsed time or rate).
    struct Link {
        /// The live playback state, or `None` before the first track is loaded.
        shared: Option<Arc<SharedState>>,
        title: String,
        duration: f32,
    }

    /// Shared between `MediaControls` and every registered handler block. Both
    /// the egui update and the MediaPlayer command handlers run on the main run
    /// loop, so this is single-threaded (`Rc`/`RefCell`, not `Arc`/`Mutex`). The
    /// handlers mutate playback here directly, so the media key works whether or
    /// not the egui frame is currently painting.
    struct PlaybackLink {
        state: RefCell<Link>,
        info_center: Retained<MPNowPlayingInfoCenter>,
        /// The app-icon artwork, added to every Now Playing update. `None` only
        /// if the embedded icon somehow fails to decode.
        artwork: Option<Retained<MPMediaItemArtwork>>,
    }

    impl PlaybackLink {
        /// Apply a media command: flip the shared pause flag and immediately
        /// republish Now Playing so Control Center reflects it even when hidden.
        fn apply(&self, kind: MediaCommand) {
            let (title, duration, shared) = {
                let state = self.state.borrow();
                let Some(shared) = state.shared.clone() else {
                    return;
                };
                (state.title.clone(), state.duration, shared)
            };
            let new_paused = match kind {
                MediaCommand::Play => false,
                MediaCommand::Pause => true,
                MediaCommand::Toggle => !shared.paused.load(Ordering::Relaxed),
            };
            shared.paused.store(new_paused, Ordering::Relaxed);
            let elapsed = shared.position_secs();
            // SAFETY: called on the main run loop with the framework singleton.
            unsafe {
                publish(
                    &self.info_center,
                    self.artwork.as_deref(),
                    &title,
                    duration,
                    elapsed,
                    !new_paused,
                )
            };
        }
    }

    /// Owns the registered command handlers and the Now Playing info center.
    /// Lives on the UI thread for the app's lifetime; the handlers it installs
    /// stay active as long as the retained targets here are alive.
    pub struct MediaControls {
        link: Rc<PlaybackLink>,
        // Retained command targets. Dropping them would unregister the handlers,
        // so they are kept for the lifetime of the app.
        _targets: Vec<Retained<AnyObject>>,
    }

    impl MediaControls {
        pub fn new() -> Self {
            // SAFETY: `defaultCenter` returns the process-wide singleton.
            let info_center = unsafe { MPNowPlayingInfoCenter::defaultCenter() };
            let link = Rc::new(PlaybackLink {
                state: RefCell::new(Link {
                    shared: None,
                    title: String::new(),
                    duration: 0.0,
                }),
                info_center,
                artwork: build_artwork(),
            });
            let mut targets = Vec::new();

            // SAFETY: standard MediaPlayer usage; the shared command center and
            // its commands are process-wide singletons, and the handler blocks
            // are `'static` (they capture only a cloneable `Arc`).
            unsafe {
                let center = MPRemoteCommandCenter::sharedCommandCenter();
                targets.push(add_handler(
                    &center.togglePlayPauseCommand(),
                    link.clone(),
                    MediaCommand::Toggle,
                ));
                targets.push(add_handler(
                    &center.playCommand(),
                    link.clone(),
                    MediaCommand::Play,
                ));
                targets.push(add_handler(
                    &center.pauseCommand(),
                    link.clone(),
                    MediaCommand::Pause,
                ));

                // hyla only exposes play/pause, so disable the commands it does
                // not implement. This keeps Control Center's transport tidy and
                // avoids dead next/previous/scrub controls.
                for command in [
                    center.nextTrackCommand(),
                    center.previousTrackCommand(),
                    center.seekForwardCommand(),
                    center.seekBackwardCommand(),
                ] {
                    command.setEnabled(false);
                }
                center.changePlaybackPositionCommand().setEnabled(false);
            }

            Self {
                link,
                _targets: targets,
            }
        }

        /// Associate the live playback state so media-key handlers can control
        /// it. Called once, when the player is first created.
        pub fn attach(&self, shared: Arc<SharedState>) {
            self.link.state.borrow_mut().shared = Some(shared);
        }

        /// Publish the current track to the system Now Playing center. Claiming
        /// this slot is also what makes the OS route the media key to hyla.
        pub fn update_now_playing(&self, title: &str, duration: f32, elapsed: f32, playing: bool) {
            {
                let mut guard = self.link.state.borrow_mut();
                guard.title = title.to_owned();
                guard.duration = duration;
            }
            // SAFETY: called on the UI/main thread with the framework singleton.
            unsafe {
                publish(
                    &self.link.info_center,
                    self.link.artwork.as_deref(),
                    title,
                    duration,
                    elapsed,
                    playing,
                )
            };
        }
    }

    /// Rebuild and push the Now Playing dictionary and playback state.
    ///
    /// # Safety
    /// Must run on the main thread; `info_center` is the framework singleton.
    unsafe fn publish(
        info_center: &MPNowPlayingInfoCenter,
        artwork: Option<&MPMediaItemArtwork>,
        title: &str,
        duration: f32,
        elapsed: f32,
        playing: bool,
    ) {
        let dict = NSMutableDictionary::<NSString, AnyObject>::new();
        let title = NSString::from_str(title);
        let duration = NSNumber::numberWithDouble(duration.max(0.0) as f64);
        let elapsed = NSNumber::numberWithDouble(elapsed.max(0.0) as f64);
        let rate = NSNumber::numberWithDouble(if playing { 1.0 } else { 0.0 });

        // SAFETY: all keys are the framework's static property strings and the
        // values are freshly created Foundation objects of the expected type.
        unsafe {
            dict.setObject_forKey(&title, key(MPMediaItemPropertyTitle));
            dict.setObject_forKey(&duration, key(MPMediaItemPropertyPlaybackDuration));
            dict.setObject_forKey(&elapsed, key(MPNowPlayingInfoPropertyElapsedPlaybackTime));
            dict.setObject_forKey(&rate, key(MPNowPlayingInfoPropertyPlaybackRate));
            if let Some(artwork) = artwork {
                dict.setObject_forKey(artwork, key(MPMediaItemPropertyArtwork));
            }

            info_center.setNowPlayingInfo(Some(&dict));
            info_center.setPlaybackState(if playing {
                MPNowPlayingPlaybackState::Playing
            } else {
                MPNowPlayingPlaybackState::Paused
            });
        }
    }

    /// Build the Now Playing artwork from the embedded app icon. Returns `None`
    /// only if the icon fails to decode, in which case Now Playing simply shows
    /// no artwork.
    fn build_artwork() -> Option<Retained<MPMediaItemArtwork>> {
        let data = NSData::with_bytes(ARTWORK_PNG);
        let image = NSImage::initWithData(NSImage::alloc(), &data)?;
        let size = image.size();
        // The system requests the artwork at various sizes; hand back the same
        // icon each time and let it scale. The block is retained by the artwork.
        let handler = RcBlock::new(move |_requested: NSSize| {
            NonNull::new(Retained::autorelease_return(image.clone()))
                .expect("cloned NSImage is never null")
        });
        // SAFETY: the bounds size comes from the image itself, and the request
        // handler always returns a valid, non-null NSImage.
        let artwork = unsafe {
            MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                MPMediaItemArtwork::alloc(),
                size,
                &handler,
            )
        };
        Some(artwork)
    }

    /// Register `kind` to fire whenever `command` is invoked, returning the
    /// retained target that keeps the handler alive. The handler acts on
    /// `link` directly so it works even while the window is hidden.
    ///
    /// # Safety
    /// `command` must be a valid `MPRemoteCommand`.
    unsafe fn add_handler(
        command: &MPRemoteCommand,
        link: Rc<PlaybackLink>,
        kind: MediaCommand,
    ) -> Retained<AnyObject> {
        let handler = RcBlock::new(move |_event: NonNull<MPRemoteCommandEvent>| {
            link.apply(kind);
            MPRemoteCommandHandlerStatus::Success
        });
        // SAFETY: the command is valid and the block outlives the registration
        // (its retained target is returned to the caller and stored).
        unsafe {
            command.setEnabled(true);
            command.addTargetWithHandler(&handler)
        }
    }

    /// A Now Playing dictionary key: the framework property strings conform to
    /// `NSCopying`, which is what `NSDictionary` requires of its keys.
    fn key(k: &NSString) -> &ProtocolObject<dyn NSCopying> {
        ProtocolObject::from_ref(k)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::sync::Arc;

    use crate::audio::shared::SharedState;

    /// Non-macOS stub: hyla is a macOS app, but keeping the surface identical
    /// lets the rest of the code stay platform-agnostic.
    pub struct MediaControls;

    impl MediaControls {
        pub fn new() -> Self {
            MediaControls
        }

        pub fn attach(&self, _shared: Arc<SharedState>) {}

        pub fn update_now_playing(
            &self,
            _title: &str,
            _duration: f32,
            _elapsed: f32,
            _playing: bool,
        ) {
        }
    }
}
