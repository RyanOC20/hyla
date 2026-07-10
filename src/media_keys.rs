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
//! Commands arrive on the main run loop, so the handlers simply forward an intent
//! over a channel that the egui thread drains each frame. hyla also publishes
//! minimal Now Playing info (filename, duration, elapsed, rate) so the OS routes
//! media keys to it and Control Center shows the current track.

/// A play/pause intent forwarded from a system media command.
#[derive(Clone, Copy, Debug)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
}

pub use imp::MediaControls;

#[cfg(target_os = "macos")]
mod imp {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use crossbeam_channel::{Receiver, Sender};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2_foundation::{NSCopying, NSMutableDictionary, NSNumber, NSString};
    use objc2_media_player::{
        MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
        MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyPlaybackRate,
        MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
        MPRemoteCommandHandlerStatus,
    };

    use super::MediaCommand;

    /// Owns the registered command handlers and the Now Playing info center.
    /// Lives on the UI thread for the app's lifetime; the handlers it installs
    /// stay active as long as the retained targets here are alive.
    pub struct MediaControls {
        rx: Receiver<MediaCommand>,
        info_center: Retained<MPNowPlayingInfoCenter>,
        // Retained command targets. Dropping them would unregister the handlers,
        // so they are kept for the lifetime of the app.
        _targets: Vec<Retained<AnyObject>>,
    }

    impl MediaControls {
        pub fn new() -> Self {
            let (tx, rx) = crossbeam_channel::unbounded();
            let mut targets = Vec::new();

            // SAFETY: standard MediaPlayer usage; the shared command center and
            // its commands are process-wide singletons, and the handler blocks
            // are `'static` (they capture only a cloneable channel sender).
            unsafe {
                let center = MPRemoteCommandCenter::sharedCommandCenter();
                targets.push(add_handler(
                    &center.togglePlayPauseCommand(),
                    tx.clone(),
                    MediaCommand::Toggle,
                ));
                targets.push(add_handler(
                    &center.playCommand(),
                    tx.clone(),
                    MediaCommand::Play,
                ));
                targets.push(add_handler(&center.pauseCommand(), tx, MediaCommand::Pause));

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

            // SAFETY: `defaultCenter` returns the process-wide singleton.
            let info_center = unsafe { MPNowPlayingInfoCenter::defaultCenter() };

            Self {
                rx,
                info_center,
                _targets: targets,
            }
        }

        /// Drain any media commands received since the last call.
        pub fn poll(&self) -> Vec<MediaCommand> {
            self.rx.try_iter().collect()
        }

        /// Publish the current track to the system Now Playing center. Claiming
        /// this slot is also what makes the OS route the media key to hyla.
        pub fn update_now_playing(&self, title: &str, duration: f32, elapsed: f32, playing: bool) {
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

                self.info_center.setNowPlayingInfo(Some(&dict));
                self.info_center.setPlaybackState(if playing {
                    MPNowPlayingPlaybackState::Playing
                } else {
                    MPNowPlayingPlaybackState::Paused
                });
            }
        }
    }

    /// Register `kind` to fire whenever `command` is invoked, returning the
    /// retained target that keeps the handler alive.
    ///
    /// # Safety
    /// `command` must be a valid `MPRemoteCommand`.
    unsafe fn add_handler(
        command: &MPRemoteCommand,
        tx: Sender<MediaCommand>,
        kind: MediaCommand,
    ) -> Retained<AnyObject> {
        let handler = RcBlock::new(move |_event: NonNull<MPRemoteCommandEvent>| {
            let _ = tx.send(kind);
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
    use super::MediaCommand;

    /// Non-macOS stub: hyla is a macOS app, but keeping the surface identical
    /// lets the rest of the code stay platform-agnostic.
    pub struct MediaControls;

    impl MediaControls {
        pub fn new() -> Self {
            MediaControls
        }

        pub fn poll(&self) -> Vec<MediaCommand> {
            Vec::new()
        }

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
