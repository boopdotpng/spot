// macOS Now Playing: what's playing, shown in Control Center and on the media keys' overlay, and
// the media keys and Control Center's buttons coming back as `MediaEvent`s. Plain objc2 calls to
// MediaPlayer; elsewhere it's all a no-op.

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub enum MediaEvent {
    Toggle,
    Play,
    Pause,
    Next,
    Previous,
    SetPosition(Duration),
}

pub struct Metadata<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub cover_url: Option<&'a str>,
    pub duration: Duration,
}

/// What's playing: nothing, or a song playing or paused at a position.
pub enum Playback {
    Stopped,
    Playing(Duration),
    Paused(Duration),
}

#[cfg(target_os = "macos")]
pub use mac::MediaControls;

#[cfg(not(target_os = "macos"))]
pub struct MediaControls;

#[cfg(not(target_os = "macos"))]
impl MediaControls {
    pub fn attach(_send: impl Fn(MediaEvent) + Clone + 'static) -> Option<Self> {
        None
    }
    pub fn set_metadata(&mut self, _m: Metadata) {}
    pub fn set_playback(&mut self, _p: Playback) {}
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{MediaEvent, Metadata, Playback};
    use block2::RcBlock;
    use objc2::rc::{Allocated, Retained, autoreleasepool};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSMutableDictionary, NSNumber, NSSize, NSString, NSURL};
    use std::ptr::NonNull;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[link(name = "MediaPlayer", kind = "framework")]
    unsafe extern "C" {
        static MPMediaItemPropertyTitle: &'static NSString;
        static MPMediaItemPropertyArtist: &'static NSString;
        static MPMediaItemPropertyAlbumTitle: &'static NSString;
        static MPMediaItemPropertyArtwork: &'static NSString;
        static MPMediaItemPropertyPlaybackDuration: &'static NSString;
        static MPNowPlayingInfoPropertyElapsedPlaybackTime: &'static NSString;
    }

    // NSImage, for the cover
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    /// Everything Now Playing shows, so each change sets all of it: setting part of it from two
    /// threads (the cover arrives on its own) would drop the other's half.
    #[derive(Default)]
    struct Info {
        song: u64, // which song this is, so a late cover for the one before is dropped
        title: String,
        artist: String,
        album: String,
        duration: f64,
        elapsed: f64,
        artwork: Option<Artwork>,
    }

    /// An `MPMediaItemArtwork`. It's only touched under the lock.
    struct Artwork(Retained<AnyObject>);
    unsafe impl Send for Artwork {}

    pub struct MediaControls {
        info: Arc<Mutex<Info>>,
    }

    impl MediaControls {
        /// Takes the media keys and Control Center's buttons. Call it on the main thread: macOS
        /// reads which commands are enabled from there, once.
        pub fn attach(send: impl Fn(MediaEvent) + Clone + 'static) -> Option<Self> {
            autoreleasepool(|_| unsafe {
                let center: *mut AnyObject = msg_send![class!(MPRemoteCommandCenter), sharedCommandCenter];
                let center = center.as_ref()?;
                let commands: [(*mut AnyObject, MediaEvent); 5] = [
                    (msg_send![center, togglePlayPauseCommand], MediaEvent::Toggle),
                    (msg_send![center, playCommand], MediaEvent::Play),
                    (msg_send![center, pauseCommand], MediaEvent::Pause),
                    (msg_send![center, nextTrackCommand], MediaEvent::Next),
                    (msg_send![center, previousTrackCommand], MediaEvent::Previous),
                ];
                for (cmd, ev) in commands {
                    let send = send.clone();
                    add_handler(cmd, RcBlock::new(move |_: NonNull<AnyObject>| -> isize {
                        send(ev);
                        0 // MPRemoteCommandHandlerStatusSuccess
                    }));
                }
                // the seek bar: an MPChangePlaybackPositionCommandEvent
                let send = send.clone();
                add_handler(msg_send![center, changePlaybackPositionCommand], RcBlock::new(move |e: NonNull<AnyObject>| -> isize {
                    let secs: f64 = msg_send![e.as_ref(), positionTime];
                    send(MediaEvent::SetPosition(Duration::from_secs_f64(secs.max(0.0))));
                    0
                }));
                Some(())
            })?;
            Some(Self { info: Arc::default() })
        }

        /// A new song. Its cover is fetched on a thread of its own and added when it arrives.
        pub fn set_metadata(&mut self, m: Metadata) {
            let song = {
                let mut i = self.info.lock().unwrap();
                i.song += 1;
                i.title = m.title.into();
                i.artist = m.artist.into();
                i.album = m.album.into();
                i.duration = m.duration.as_secs_f64();
                i.elapsed = 0.0;
                i.artwork = None;
                publish(&i);
                i.song
            };
            let Some(url) = m.cover_url.map(str::to_owned) else { return };
            let info = self.info.clone();
            std::thread::spawn(move || {
                let Some(art) = autoreleasepool(|_| artwork(&url)) else { return };
                let mut i = info.lock().unwrap();
                if i.song == song {
                    i.artwork = Some(art);
                    publish(&i);
                }
            });
        }

        pub fn set_playback(&mut self, p: Playback) {
            let (state, at) = match p {
                Playback::Playing(at) => (1usize, Some(at)), // MPNowPlayingPlaybackStatePlaying
                Playback::Paused(at) => (2, Some(at)),
                Playback::Stopped => (3, None),
            };
            let mut i = self.info.lock().unwrap();
            autoreleasepool(|_| unsafe {
                let _: () = msg_send![&*center(), setPlaybackState: state];
            });
            if let Some(at) = at {
                i.elapsed = at.as_secs_f64();
                publish(&i);
            }
        }
    }

    /// Turn a command on, running `block` when it comes.
    unsafe fn add_handler(cmd: *mut AnyObject, block: RcBlock<dyn Fn(NonNull<AnyObject>) -> isize>) {
        let Some(cmd) = (unsafe { cmd.as_ref() }) else { return };
        let _: () = msg_send![cmd, setEnabled: Bool::YES];
        // the command keeps its own copy of the block; the target it returns is the command's too
        let _: *mut AnyObject = msg_send![cmd, addTargetWithHandler: &*block];
    }

    fn center() -> Retained<AnyObject> {
        unsafe { msg_send![class!(MPNowPlayingInfoCenter), defaultCenter] }
    }

    /// Hand all of `i` to Now Playing.
    fn publish(i: &Info) {
        autoreleasepool(|_| unsafe {
            let d: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
            let set = |k: &NSString, v: &AnyObject| {
                let _: () = msg_send![&*d, setObject: v, forKey: k];
            };
            set(MPMediaItemPropertyTitle, &NSString::from_str(&i.title));
            set(MPMediaItemPropertyArtist, &NSString::from_str(&i.artist));
            set(MPMediaItemPropertyAlbumTitle, &NSString::from_str(&i.album));
            set(MPMediaItemPropertyPlaybackDuration, &NSNumber::new_f64(i.duration));
            set(MPNowPlayingInfoPropertyElapsedPlaybackTime, &NSNumber::new_f64(i.elapsed));
            if let Some(a) = &i.artwork {
                set(MPMediaItemPropertyArtwork, &a.0);
            }
            let _: () = msg_send![&*center(), setNowPlayingInfo: &*d];
        });
    }

    /// The cover at `url`, fetched and decoded, as an `MPMediaItemArtwork`.
    fn artwork(url: &str) -> Option<Artwork> {
        unsafe {
            let url = NSURL::URLWithString(&NSString::from_str(url))?;
            let a: Allocated<AnyObject> = msg_send![class!(NSImage), alloc];
            let image: Option<Retained<AnyObject>> = msg_send![a, initWithContentsOfURL: &*url];
            let image = image?;
            let size: NSSize = msg_send![&*image, size];
            // the block owns the image, and the artwork the block: both go when the artwork does
            let handler = RcBlock::new(move |_: NSSize| -> *mut AnyObject { Retained::as_ptr(&image) as *mut AnyObject });
            let a: Allocated<AnyObject> = msg_send![class!(MPMediaItemArtwork), alloc];
            let art: Option<Retained<AnyObject>> = msg_send![a, initWithBoundsSize: size, requestHandler: &*handler];
            art.map(Artwork)
        }
    }
}
