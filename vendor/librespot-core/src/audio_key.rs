use std::{
    collections::HashMap,
    io::Write,
    time::{Duration, Instant},
};

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use bytes::Bytes;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{Error, FileId, SpotifyId, packet::PacketType, util::SeqGenerator};

#[derive(Debug, Hash, PartialEq, Eq, Copy, Clone)]
pub struct AudioKey(pub [u8; 16]);

#[derive(Debug, Error)]
pub enum AudioKeyError {
    #[error("audio key error")]
    AesKey,
    #[error("other end of channel disconnected")]
    Channel,
    #[error("unexpected packet type {0}")]
    Packet(u8),
    #[error("sequence {0} not pending")]
    Sequence(u32),
    #[error("audio key response timeout")]
    Timeout,
}

impl From<AudioKeyError> for Error {
    fn from(err: AudioKeyError) -> Self {
        match err {
            AudioKeyError::AesKey => Error::unavailable(err),
            AudioKeyError::Channel => Error::aborted(err),
            AudioKeyError::Sequence(_) => Error::aborted(err),
            AudioKeyError::Packet(_) => Error::unimplemented(err),
            AudioKeyError::Timeout => Error::aborted(err),
        }
    }
}

component! {
    AudioKeyManager : AudioKeyManagerInner {
        sequence: SeqGenerator<u32> = SeqGenerator::new(0),
        pending: HashMap<u32, oneshot::Sender<Result<AudioKey, [u8; 2]>>> = HashMap::new(),
        given_up: HashMap<FileId, Instant> = HashMap::new(),
        next_waiter: u64 = 0,
        waiting: Vec<u64> = Vec::new(),
    }
}

// spot patch: Spotify rate-limits key requests per account (a burst of about 8, then roughly one
// every 30s) and answers error 0 2 when it's out; refused requests don't seem to count. Keys are
// cached on disk, and a refused or timed-out request is retried for up to KEY_RETRY_FOR instead of
// failing the track. Other codes (0 1: never allowed) fail right away. After giving up on a file,
// asking again within KEY_GIVEN_UP_FOR fails at once: the player asks twice per failed load.
// A request retrying a refusal stops once a newer one is refused too: the newer one is for the
// track skipped to, and the next key Spotify hands out should go to it, not one skipped past.
const KEY_RESPONSE_TIMEOUT: Duration = Duration::from_millis(1500);
const KEY_RETRY_FOR: Duration = Duration::from_secs(40);
const KEY_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const KEY_GIVEN_UP_FOR: Duration = Duration::from_secs(10);
const KEY_ERROR_TRANSIENT: [u8; 2] = [0, 2];

/// spot patch: a request retrying refusals, in `waiting` until it's dropped.
struct Waiter<'a> {
    keys: &'a AudioKeyManager,
    id: u64,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let id = self.id;
        self.keys.lock(|inner| inner.waiting.retain(|&w| w != id));
    }
}

impl AudioKeyManager {
    pub(crate) fn dispatch(&self, cmd: PacketType, mut data: Bytes) -> Result<(), Error> {
        let seq = BigEndian::read_u32(data.split_to(4).as_ref());

        let sender = self
            .lock(|inner| inner.pending.remove(&seq))
            .ok_or(AudioKeyError::Sequence(seq))?;

        match cmd {
            PacketType::AesKey => {
                let mut key = [0u8; 16];
                key.copy_from_slice(data.as_ref());
                sender
                    .send(Ok(AudioKey(key)))
                    .map_err(|_| AudioKeyError::Channel)?
            }
            PacketType::AesKeyError => sender
                .send(Err([data.as_ref()[0], data.as_ref()[1]]))
                .map_err(|_| AudioKeyError::Channel)?,
            _ => {
                trace!("Did not expect {cmd:?} AES key packet with data {data:#?}");
                return Err(AudioKeyError::Packet(cmd as u8).into());
            }
        }

        Ok(())
    }

    pub async fn request(&self, track: SpotifyId, file: FileId) -> Result<AudioKey, Error> {
        let cache = self.session().cache().cloned();
        if let Some(key) = cache.as_ref().and_then(|c| c.audio_key(file)) {
            return Ok(AudioKey(key));
        }
        let recently_given_up = self.lock(|inner| {
            inner.given_up.retain(|_, t| t.elapsed() < KEY_GIVEN_UP_FOR);
            inner.given_up.contains_key(&file)
        });
        if recently_given_up {
            return Err(AudioKeyError::AesKey.into());
        }
        let start = Instant::now();
        let mut delay = Duration::from_millis(250);
        let mut waiter: Option<Waiter> = None;
        loop {
            let err = match self.request_once(track, file).await? {
                Ok(key) => {
                    if let Some(c) = &cache {
                        c.save_audio_key(file, &key.0);
                    }
                    return Ok(key);
                }
                Err(Some(code)) if code != KEY_ERROR_TRANSIENT => {
                    error!("error audio key {:x} {:x}", code[0], code[1]);
                    return Err(AudioKeyError::AesKey.into());
                }
                Err(code) => code,
            };
            let id = waiter.get_or_insert_with(|| {
                let id = self.lock(|inner| {
                    let id = inner.next_waiter;
                    inner.next_waiter += 1;
                    inner.waiting.push(id);
                    id
                });
                Waiter { keys: self, id }
            }).id;
            // given up, as below: the player's second request for this file mustn't count as newer
            if self.lock(|inner| {
                let newer = inner.waiting.iter().any(|&w| w > id);
                if newer {
                    inner.given_up.insert(file, Instant::now());
                }
                newer
            }) {
                warn!("error audio key, giving way to a newer request");
                return Err(AudioKeyError::AesKey.into());
            }
            if start.elapsed() + delay > KEY_RETRY_FOR {
                self.lock(|inner| inner.given_up.insert(file, Instant::now()));
                return Err(match err {
                    Some(code) => {
                        error!("error audio key {:x} {:x}, giving up", code[0], code[1]);
                        AudioKeyError::AesKey
                    }
                    None => {
                        error!("Audio key response timeout, giving up");
                        AudioKeyError::Timeout
                    }
                }
                .into());
            };
            match err {
                Some(code) => warn!("error audio key {:x} {:x}, retrying in {delay:?}", code[0], code[1]),
                None => warn!("Audio key response timeout, retrying in {delay:?}"),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(KEY_RETRY_MAX_DELAY);
        }
    }

    /// One key request: Ok(Ok(key)), Ok(Err(Some(error code))), or Ok(Err(None)) on a timeout.
    async fn request_once(
        &self,
        track: SpotifyId,
        file: FileId,
    ) -> Result<Result<AudioKey, Option<[u8; 2]>>, Error> {
        let (tx, rx) = oneshot::channel();

        let seq = self.lock(move |inner| {
            let seq = inner.sequence.get();
            inner.pending.insert(seq, tx);
            seq
        });

        if let Err(e) = self.send_key_request(seq, track, file) {
            self.lock(|inner| inner.pending.remove(&seq));
            return Err(e);
        }
        match tokio::time::timeout(KEY_RESPONSE_TIMEOUT, rx).await {
            Err(_) => {
                self.lock(|inner| inner.pending.remove(&seq));
                Ok(Err(None))
            }
            Ok(Err(_)) => Err(AudioKeyError::Channel.into()),
            Ok(Ok(r)) => Ok(r.map_err(Some)),
        }
    }

    fn send_key_request(&self, seq: u32, track: SpotifyId, file: FileId) -> Result<(), Error> {
        let mut data: Vec<u8> = Vec::new();
        data.write_all(&file.0)?;
        data.write_all(&track.to_raw())?;
        data.write_u32::<BigEndian>(seq)?;
        data.write_u16::<BigEndian>(0x0000)?;

        self.session().send_packet(PacketType::RequestKey, data)
    }
}
