use crate::error::*;
use crate::metainfo::Metainfo;
use crate::peer::PeerSession;
use crate::piece_picker::PiecePicker;
use crate::{PeerId, Sha1Hash};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task;
use tokio::time;
use std::time::{Instant, Duration};
use futures::StreamExt;

struct TorrentInfo {
    // Information that is shared with peer sessions.
    shared: Arc<SharedTorrentInfo>,
    // The time the torrent was first started.
    start_time: Option<Instant>,
    // The total time the torrent has been running.
    //
    // This is a separate field as `Instant::now() - start_time` cannot be
    // relied upon due to the fact that it is possible to pause a torrent, in
    // which case we don't want to record the run time.
    //
    // TODO: pausing a torrent is not actually at this point, but this is done
    // in expectation of that feature
    run_duration: Duration,
}

// Information shared with peer sessions.
//
// This type contains fields that need to be read or updated by peer sessions.
// Fields expected to be mutated are thus secured for inter-task access with
// various synchronization primitives.
pub(crate) struct SharedTorrentInfo {
    // The info hash of the torrent, derived from its metainfo. This is used to
    // identify the torrent with other peers and trackers.
    pub info_hash: Sha1Hash,
    // The arbitrary client id, chosen by the user of this library. This is
    // advertised to peers and trackers.
    pub client_id: PeerId,
}

// Peer metadata tracked by torrent.
struct Peer {
    // The address of a single peer this torrent donwloads the file from. This
    // peer has to be a seed as currently we only support downloading and no
    // seeding.
    addr: SocketAddr,
    // The join handle to the peer session task. This is set when the session is
    // started.
    handle: Option<task::JoinHandle<Result<()>>>,
}

pub struct Torrent {
    seed: Peer,
    // General information about a torrent.
    info: TorrentInfo,
    // This is passed to peer and tracks the availability of our pieces as well
    // as pieces in the torrent swarm (more relevant when more peers are added),
    // and using this knowledge which piece to pick next.
    piece_picker: Arc<RwLock<PiecePicker>>,
}

impl Torrent {
    pub fn new(
        client_id: PeerId,
        metainfo: Metainfo,
        seed_addr: SocketAddr,
    ) -> Result<Self> {
        let info_hash = metainfo.create_info_hash()?;
        let info = TorrentInfo {
            shared: Arc::new(SharedTorrentInfo { info_hash, client_id }),
            start_time: None,
            run_duration: Duration::default(),
        };

        let piece_picker = PiecePicker::new(metainfo.piece_count());
        let piece_picker = Arc::new(RwLock::new(piece_picker));

        Ok(Self {
            seed: Peer {
                addr: seed_addr,
                handle: None,
            },
            info,
            piece_picker,
        })
    }

    pub async fn start(&mut self) {
        log::info!("Starting torrent");

        // record the torrent starttime
        self.info.start_time = Some(Instant::now());

        // start the seed peer session
        let mut session = PeerSession::outbound(
            Arc::clone(&self.info.shared),
            Arc::clone(&self.piece_picker),
            self.seed.addr,
        );
        let handle = task::spawn(async move { session.start().await });
        self.seed.handle = Some(handle);

        // start the torrent update loop
        let update_loop = async {
            let mut loop_timer = time::interval(Duration::from_secs(1));
            let mut prev_instant = None;
            while let Some(instant) = loop_timer.next().await {
                // calculate how long torrent has been running
                //
                // only deal with std time types
                let instant = instant.into_std();
                let elapsed = if let Some(prev_instant) = prev_instant {
                    instant.saturating_duration_since(prev_instant)
                } else if let Some(start_time) = self.info.start_time {
                    instant.saturating_duration_since(start_time)
                } else {
                    Duration::default()
                };
                self.info.run_duration += elapsed;
                prev_instant = Some(instant);

                log::debug!("Torrent running for {}s", self.info.run_duration.as_secs());
            }
        };
        update_loop.await
    }
}
