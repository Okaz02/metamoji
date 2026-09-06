//! Fetching what a note's room already holds.
//!
//! A class note's own file, as the drive hands it out, contains only what the
//! teacher put there: every student's writing lives in the room, on the booth
//! for their personal layer. Opening the note therefore means asking the room
//! for its history — `AttachBooth` with `last:0` replays a booth from the
//! beginning — and folding the result into the document.
//!
//! This is a *pull*, not a session: it joins, collects, and leaves. The relay
//! never says "that is all", so the end is decided by silence — nothing for
//! `QUIET`, or `LIMIT` overall, whichever comes first. A live session that
//! stays joined is a different thing and belongs in `session.rs`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use super::apply::{self, Applied};
use super::send;
use super::session::ClassroomState;
use super::socket::{self, CollaboEvent, Command};
use crate::cloud::CloudClient;
use crate::error::AppResult;
use crate::model::GenericTree;

/// A booth id and the bytes that arrived on it.
type Received = Arc<Mutex<Vec<(String, i64, Vec<u8>)>>>;
/// A ticket, its media type, and its bytes.
type Asset = (String, String, Vec<u8>);
/// A booth and the bytes to post to it.
pub type Frame = (String, Vec<u8>);

/// No traffic for this long means the replay is over.
const QUIET: Duration = Duration::from_millis(1500);
/// However busy the room, stop asking after this.
const LIMIT: Duration = Duration::from_secs(20);
/// How long to wait for `LoginRoomResult` before giving up on the room.
const LOGIN_LIMIT: Duration = Duration::from_secs(10);
/// How long to wait for the relay to answer the frames just posted.
const ACK_LIMIT: Duration = Duration::from_secs(10);
/// How often the two waits above look at what has arrived.
const TICK: Duration = Duration::from_millis(50);

/// What a pull found, for the user rather than for the log.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomPull {
    pub directions: usize,
    /// How far each booth was read, so the next connection can carry on.
    #[serde(skip)]
    pub marks: Vec<(String, i64)>,
    /// Everything the room replayed, kept so a resync can work out what the
    /// room actually holds without asking twice.
    #[serde(skip)]
    pub history: Vec<(String, i64, Vec<u8>)>,
    pub units: usize,
    pub strokes: usize,
    /// Elements the room says have been erased since.
    pub removed: usize,
    pub assets: usize,
    /// Strokes now in the note, by the id the room knows them by, and the
    /// layer they are on. The caller records these so a later erase can be
    /// reported.
    #[serde(skip)]
    pub stroke_ids: Vec<(String, String)>,
    /// Edit kinds this build received but does not apply, named and counted.
    pub unsupported: Vec<String>,
    /// Set when the room could not be reached. The note still opens.
    pub error: Option<String>,
}

/// The booths a page keeps its content on.
///
/// The page itself, its common layer, and — depending on how the note was
/// distributed — the layer that belongs to this user. `NsDirectionManager
/// .boothIdArrayOnPage` builds the same list, and `NtPageController` builds
/// the ids: `{pageId}_[layer-common]`, `{pageId}_[layer-forUser]_{userId}`,
/// `{pageId}_[layer-forClass]`.
pub fn booths_for(tree: &GenericTree, user_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    for model in tree.models.values() {
        if model.model_type != "$page" {
            continue;
        }
        let Some(page_id) = model.props.get("pageId").and_then(|v| v.as_str()) else {
            continue;
        };
        out.push(page_id.to_string());
        out.push(format!("{page_id}_[layer-common]"));

        // `forSchoolPageType`: 1 per user, 2 per group, 3 per class.
        match model
            .props
            .get("forSchoolPageType")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
        {
            1 => out.push(format!("{page_id}_[layer-forUser]_{user_id}")),
            3 => out.push(format!("{page_id}_[layer-forClass]")),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Posts strokes and non-ink units into the room in one connection.
///
/// Joins, sends, leaves — the same shape as `fetch`, and for the same reason:
/// a session that stays open belongs to the editor, not to a one-off write.
/// Both kinds of change go out over the *same* connection deliberately: the
/// relay allows one connection per device, so opening a second right after
/// the first disconnects — which is what two separate send calls did, one
/// for strokes and one for units — races the first one's teardown instead of
/// waiting for it, and that race is what made sends after a save unreliable.
///
/// Returns only what the relay actually acknowledged. Nothing here counts as
/// sent on the strength of having been handed to a socket.
pub async fn post_all(
    cloud: &CloudClient,
    classroom: &ClassroomState,
    room_id: &str,
    strokes: &[send::Pending],
    removals: &[send::Ledger],
    units: &[send::PendingUnit],
) -> AppResult<Vec<Record>> {
    if strokes.is_empty() && removals.is_empty() && units.is_empty() {
        return Ok(Vec::new());
    }
    let Some(session) = cloud.session() else {
        return Err(crate::error::AppError::other("サインインしていません"));
    };

    let device_id = classroom.device_id(cloud).await?;
    let relay = classroom
        .rest(cloud)
        .await?
        .login_room(room_id, None)
        .await?;

    let login: LoginSlot = Default::default();
    let acks: AckTally = Default::default();
    let watching_login = Arc::clone(&login);
    let watching_acks = Arc::clone(&acks);
    let connection = socket::connect(&relay.host, relay.port, move |event| {
        note_login(&watching_login, &event);
        note_ack(&watching_acks, &event);
    })
    .await?;

    connection
        .commands
        .send(Command::Login {
            room_id: room_id.to_string(),
            device_id,
            session_id: relay.session_id.clone(),
            nickname: session.name.clone(),
        })
        .await
        .ok();
    // The room hands out its own id for us on login, and stamps it on every
    // element. Until it arrives there is nothing honest to put there — and
    // nowhere to post to either, because the relay ignores what it is sent
    // before it considers the connection to be in the room.
    let room_user_id = await_login(&login).await?;

    let author = author_of(&session, room_id, room_user_id.unwrap_or_default());
    let mut batches = build_posts(strokes, removals, &author)?;
    batches.extend(build_unit_posts(units, &author)?);
    let confirmed = post_batches(&connection, &acks, &batches).await;

    let _ = connection
        .commands
        .send(Command::Logout {
            room_id: room_id.to_string(),
        })
        .await;
    let _ = connection.commands.send(Command::Disconnect).await;
    Ok(confirmed)
}

/// The room's answer to `LoginRoom`, once it has given one.
#[derive(Debug, Clone)]
pub enum LoginAnswer {
    Accepted { room_user_id: Option<String> },
    Refused { message: Option<String> },
}

/// Where a socket's event handler leaves that answer for whoever is waiting.
pub type LoginSlot = Arc<Mutex<Option<LoginAnswer>>>;

pub fn note_login(slot: &LoginSlot, event: &CollaboEvent) {
    let CollaboEvent::LoggedIn {
        ok,
        message,
        user_id,
        ..
    } = event
    else {
        return;
    };
    *slot.lock().unwrap() = Some(if *ok {
        LoginAnswer::Accepted {
            room_user_id: user_id.clone(),
        }
    } else {
        LoginAnswer::Refused {
            message: message.clone(),
        }
    });
}

/// Waits for the room to answer `LoginRoom` rather than guessing how long it
/// will take.
///
/// A fixed grace period used to stand here, and it is a large part of why
/// sending looked random: the relay ignores what it is posted before it
/// considers the connection to be in the room, so a login slower than the
/// guess lost every frame that followed it — silently, because nothing ever
/// read the answer. A refused login was worse still: the posts went nowhere
/// and the send reported success anyway.
pub async fn await_login(slot: &LoginSlot) -> AppResult<Option<String>> {
    let deadline = std::time::Instant::now() + LOGIN_LIMIT;
    loop {
        let answer = slot.lock().unwrap().clone();
        match answer {
            Some(LoginAnswer::Accepted { room_user_id }) => return Ok(room_user_id),
            Some(LoginAnswer::Refused { message }) => {
                return Err(crate::error::AppError::other(match message {
                    Some(msg) => format!("教室に入れませんでした: {msg}"),
                    None => "教室に入れませんでした".to_string(),
                }))
            }
            None if std::time::Instant::now() >= deadline => {
                return Err(crate::error::AppError::other(
                    "教室サーバーから応答がありません",
                ))
            }
            None => tokio::time::sleep(TICK).await,
        }
    }
}

/// Every `PostDataResult` a connection has been sent, in the order they
/// arrived: `true` for accepted, `false` for refused.
///
/// The relay answers every `PostData` (§4 of the protocol spec), and answers
/// them in the order it received them, which is what makes a plain list
/// enough: a batch is confirmed by the entries covering its own frames. The
/// packet number the answer carries cannot be used instead — the writer task
/// owns that counter, so the caller never learns which number its frame got.
pub type AckTally = Arc<Mutex<Vec<bool>>>;

pub fn note_ack(tally: &AckTally, event: &CollaboEvent) {
    if let CollaboEvent::PostAck { ok, .. } = event {
        tally.lock().unwrap().push(*ok);
    }
}

/// Posts batches down a connection and hands back the ones the relay
/// confirmed.
///
/// Posting stops at the first frame the socket will not even take: the writer
/// task refuses only once it is gone, so everything after it would be lost
/// too, and carrying on would be queueing into a closed channel.
pub async fn post_batches(
    connection: &socket::Connection,
    tally: &AckTally,
    batches: &[Batch],
) -> Vec<Record> {
    post_batches_within(connection, tally, batches, ACK_LIMIT).await
}

async fn post_batches_within(
    connection: &socket::Connection,
    tally: &AckTally,
    batches: &[Batch],
    limit: Duration,
) -> Vec<Record> {
    // Where the tally stood before any of this went out, so another poster's
    // traffic on a shared connection is not mistaken for our own.
    let start = tally.lock().unwrap().len();

    // Each batch's own frames, as a range within what this call posted.
    let mut ranges: Vec<(usize, usize, &Record)> = Vec::new();
    let mut posted = 0usize;
    for batch in batches {
        let from = posted;
        let mut queued = 0;
        for (booth_id, payload) in &batch.frames {
            if !post(connection, booth_id, payload.clone()).await {
                break;
            }
            queued += 1;
        }
        posted += queued;
        if queued < batch.frames.len() {
            break;
        }
        ranges.push((from, posted, &batch.record));
    }
    if posted == 0 {
        return Vec::new();
    }

    let deadline = std::time::Instant::now() + limit;
    while tally.lock().unwrap().len() < start + posted {
        if std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(TICK).await;
    }

    let answers = tally.lock().unwrap().clone();
    ranges
        .into_iter()
        .filter(|(_, to, _)| answers.len() >= start + to)
        .filter(|(from, to, _)| answers[start + from..start + to].iter().all(|ok| *ok))
        .map(|(_, _, record)| record.clone())
        .collect()
}

/// One thing on its way to the room, and the frames it takes to get there.
///
/// A batch is all-or-nothing on purpose. A rasterised unit is two frames —
/// its bytes, and then the unit that places them — and writing it down when
/// only the first arrived would leave the room showing nothing while this app
/// believed it had been told. The ledger is what stops something ever being
/// sent again, so a row recorded for a post that did not land is permanent,
/// and that is how writing came to be missing from the classroom at random.
#[derive(Debug, Clone)]
pub struct Batch {
    pub frames: Vec<Frame>,
    pub record: Record,
}

/// What to write down once a batch has actually reached the relay.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    /// A stroke the room now holds, under the id it was sent as.
    Stroke {
        stroke_id: String,
        element_id: String,
        layer_id: String,
    },
    /// A stroke the room has been told to drop; its ledger row goes with it.
    Erased { stroke_id: String },
    /// A non-ink unit the room now holds.
    Unit {
        unit_id: String,
        element_id: String,
        layer_id: String,
    },
}

/// Builds the frames a set of changes turns into, without sending anything.
///
/// Shared by the two ways out: a note that is open has a connection already —
/// posting down a second one and then logging out takes the first one with it,
/// because the relay ends the room session for the whole device.
pub fn build_posts(
    strokes: &[send::Pending],
    removals: &[send::Ledger],
    author: &send::Author,
) -> AppResult<Vec<Batch>> {
    let mut ids = send::IdGenerator::fresh();
    let mut batches = Vec::new();

    for pending in strokes {
        let (payload, element_id) =
            send::add_stroke(&pending.stroke, &pending.layer_id, &mut ids, author, None)?;
        batches.push(Batch {
            frames: vec![(pending.layer_id.clone(), payload)],
            record: Record::Stroke {
                stroke_id: pending.stroke_id.clone(),
                element_id,
                layer_id: pending.layer_id.clone(),
            },
        });
    }
    for entry in removals {
        let payload = send::remove_element(&entry.element_id, &entry.layer_id, &mut ids, None)?;
        batches.push(Batch {
            frames: vec![(entry.layer_id.clone(), payload)],
            // Keyed the way the ledger is, so the caller drops the right row.
            record: Record::Erased {
                stroke_id: entry.stroke_id.clone(),
            },
        });
    }
    Ok(batches)
}

/// Builds the frames posting a set of units turns into, without sending
/// anything — `build_posts`'s counterpart for `send::PendingUnit`.
///
/// Unlike ink, a unit has no erase path here: removing a shape or a text box
/// from the room is not implemented, so a unit taken out locally stays in the
/// room until this is revisited.
pub fn build_unit_posts(
    units: &[send::PendingUnit],
    author: &send::Author,
) -> AppResult<Vec<Batch>> {
    let mut ids = send::IdGenerator::fresh();
    let mut batches = Vec::new();

    for unit in units {
        let (payloads, element_id) = send::build_unit_post(unit, &mut ids, author)?;
        batches.push(Batch {
            // Both frames of a rasterised unit, kept together: the bytes are
            // no use to the room without the unit that places them, and the
            // unit shows nothing without the bytes.
            frames: payloads
                .into_iter()
                .map(|payload| (unit.layer_id().to_string(), payload))
                .collect(),
            record: Record::Unit {
                unit_id: unit.unit_id().to_string(),
                element_id,
                layer_id: unit.layer_id().to_string(),
            },
        });
    }
    Ok(batches)
}

/// The author stamp the room puts on every element.
pub fn author_of(
    session: &crate::cloud::CloudSession,
    room_id: &str,
    room_user_id: String,
) -> send::Author {
    send::Author {
        user_id: session.user_id.clone(),
        name: session.name.clone(),
        company_id: session.company_id.clone().unwrap_or_default(),
        room_id: room_id.to_string(),
        room_user_id,
    }
}

/// Hands one frame to the writer task. `false` means the socket is gone —
/// which used to be thrown away, so a note whose watch had quietly died went
/// on "sending" into a closed channel and writing every stroke down as sent.
pub(crate) async fn post(
    connection: &socket::Connection,
    booth_id: &str,
    payload: Vec<u8>,
) -> bool {
    connection
        .commands
        .send(Command::PostData {
            booth_id: booth_id.to_string(),
            payload,
            // No echo — we already have it. `save` is the whole point: it is
            // what makes the relay keep it for whoever opens the note next,
            // this user included.
            send_back: false,
            save: true,
            rip_off_size: "0".to_string(),
        })
        .await
        .is_ok()
}

/// Joins the room, replays every booth, and folds the result into `tree`.
///
/// Assets come back rather than being written here: they belong in the note's
/// store, which is the caller's to open.
pub async fn fetch(
    cloud: &CloudClient,
    classroom: &ClassroomState,
    room_id: &str,
    tree: &mut GenericTree,
    ledger: &[send::Ledger],
) -> AppResult<(RoomPull, Vec<Asset>)> {
    let Some(session) = cloud.session() else {
        return Ok((
            RoomPull {
                error: Some("サインインしていません".into()),
                ..Default::default()
            },
            Vec::new(),
        ));
    };

    let booths = booths_for(tree, &session.user_id);
    if booths.is_empty() {
        return Ok((RoomPull::default(), Vec::new()));
    }

    let device_id = classroom.device_id(cloud).await?;
    let relay = classroom
        .rest(cloud)
        .await?
        .login_room(room_id, None)
        .await?;

    let received: Received = Arc::new(Mutex::new(Vec::new()));
    let login: LoginSlot = Default::default();
    let seen = Arc::clone(&received);
    let watching_login = Arc::clone(&login);
    let connection = socket::connect(&relay.host, relay.port, move |event| {
        note_login(&watching_login, &event);
        if let CollaboEvent::Direction {
            booth_id,
            sequence,
            payload,
            ..
        } = event
        {
            seen.lock().unwrap().push((booth_id, sequence, payload));
        }
    })
    .await?;

    connection
        .commands
        .send(Command::Login {
            room_id: room_id.to_string(),
            device_id,
            session_id: relay.session_id.clone(),
            nickname: session.name.clone(),
        })
        .await
        .ok();
    // Booths are asked for only once the room says we are in it. Attaching
    // before that is answered with nothing, and the note then opens as though
    // the classroom held none of this student's work — which a resync would
    // go on to believe, and "repair" by throwing the ledger away.
    if let Err(err) = await_login(&login).await {
        return Ok((
            RoomPull {
                error: Some(err.to_string()),
                ..Default::default()
            },
            Vec::new(),
        ));
    }

    for booth in &booths {
        connection
            .commands
            .send(Command::AttachBooth {
                booth_id: booth.clone(),
                last_sequence: 0,
            })
            .await
            .ok();
    }

    // Wait for the replay to go quiet.
    let started = std::time::Instant::now();
    let mut last_count = 0usize;
    let mut quiet_since = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let count = received.lock().unwrap().len();
        if count != last_count {
            last_count = count;
            quiet_since = std::time::Instant::now();
        }
        if quiet_since.elapsed() >= QUIET || started.elapsed() >= LIMIT {
            break;
        }
    }

    let _ = connection
        .commands
        .send(Command::Logout {
            room_id: room_id.to_string(),
        })
        .await;
    let _ = connection.commands.send(Command::Disconnect).await;

    let marks = {
        let mut highest: std::collections::HashMap<String, i64> = Default::default();
        for (booth_id, sequence, _) in received.lock().unwrap().iter() {
            let mark = highest.entry(booth_id.clone()).or_insert(*sequence);
            *mark = (*mark).max(*sequence);
        }
        highest.into_iter().collect::<Vec<_>>()
    };
    let aliases: std::collections::HashMap<String, String> = ledger
        .iter()
        .map(|l| (l.element_id.clone(), l.stroke_id.clone()))
        .collect();
    let directions = std::mem::take(&mut *received.lock().unwrap());
    let (mut pull, assets) = fold(tree, directions.clone(), &aliases);
    pull.marks = marks;
    pull.history = directions;
    let result = (pull, assets);

    // Whether or not the room had anything, this user needs a layer of their
    // own to write on. Without one the next stroke lands on a layer the room
    // has no booth for and can never be sent.
    for booth in booths.iter().filter(|b| b.contains("_[layer-for")) {
        apply::ensure_booth_layer(tree, booth);
    }
    Ok(result)
}

/// What the room currently holds, worked out from its whole history.
///
/// `added` minus `removed` is what is actually there. Both are needed: a
/// ledger row for an element the room never saw means a post that did not
/// land, and one for an element it has since dropped means someone erased it.
/// They call for opposite repairs, and only the history tells them apart.
#[derive(Debug, Default, Clone)]
pub struct RoomState {
    pub added: std::collections::HashSet<String>,
    pub removed: std::collections::HashSet<String>,
}

impl RoomState {
    pub fn holds(&self, element_id: &str) -> bool {
        self.added.contains(element_id) && !self.removed.contains(element_id)
    }
}

/// Reads the room's history without touching any note.
pub fn survey(directions: &[(String, i64, Vec<u8>)]) -> RoomState {
    let mut state = RoomState::default();
    for (_, _, payload) in directions {
        let Ok(direction) = apply::decode(payload) else {
            continue;
        };
        for change in direction.changes {
            match change {
                apply::Change::Stroke { id, .. } => {
                    state.added.insert(id);
                }
                apply::Change::Remove { id } => {
                    state.removed.insert(id);
                }
                _ => {}
            }
        }
    }
    state
}

/// Applies what came back, in the order it came back.
fn fold(
    tree: &mut GenericTree,
    directions: Vec<(String, i64, Vec<u8>)>,
    aliases: &std::collections::HashMap<String, String>,
) -> (RoomPull, Vec<Asset>) {
    let mut pull = RoomPull {
        directions: directions.len(),
        ..Default::default()
    };
    let mut assets = Vec::new();
    let mut unsupported: std::collections::BTreeMap<String, usize> = Default::default();

    for (booth_id, _sequence, payload) in directions {
        let Ok(direction) = apply::decode(&payload) else {
            *unsupported
                .entry("読み取れない Direction".into())
                .or_default() += 1;
            continue;
        };
        let Applied {
            units,
            strokes,
            removed,
            stroke_ids,
            assets: found,
            unsupported: kinds,
        } = apply::apply(tree, &booth_id, &direction, aliases);
        pull.units += units;
        pull.strokes += strokes;
        pull.removed += removed;
        pull.stroke_ids.extend(stroke_ids);
        assets.extend(found);
        for kind in kinds {
            *unsupported.entry(kind).or_default() += 1;
        }
    }

    pull.assets = assets.len();
    pull.unsupported = unsupported
        .into_iter()
        .map(|(kind, count)| format!("{kind} x{count}"))
        .collect();
    (pull, assets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GenericModel;
    use serde_json::json;

    fn page(id: &str, page_type: i64) -> GenericModel {
        GenericModel {
            id: id.into(),
            parent_id: Some("root".into()),
            model_type: "$page".into(),
            props: json!({ "pageId": id, "forSchoolPageType": page_type }),
            children: Vec::new(),
        }
    }

    #[test]
    fn a_per_user_page_asks_for_this_users_own_layer() {
        let mut tree = GenericTree::new("root", "$sharenote");
        tree.insert(page("P1", 1));
        assert_eq!(
            booths_for(&tree, "42"),
            vec![
                "P1".to_string(),
                "P1_[layer-common]".to_string(),
                "P1_[layer-forUser]_42".to_string(),
            ]
        );
    }

    #[test]
    fn a_class_wide_page_asks_for_the_shared_layer_instead() {
        let mut tree = GenericTree::new("root", "$sharenote");
        tree.insert(page("P1", 3));
        assert!(booths_for(&tree, "42").contains(&"P1_[layer-forClass]".to_string()));
        assert!(!booths_for(&tree, "42")
            .iter()
            .any(|b| b.contains("forUser")));
    }

    #[test]
    fn an_ordinary_page_asks_only_for_itself_and_the_common_layer() {
        let mut tree = GenericTree::new("root", "$sharenote");
        tree.insert(page("P1", 0));
        assert_eq!(booths_for(&tree, "42").len(), 2);
    }

    #[test]
    fn a_note_with_no_pages_asks_for_nothing() {
        let tree = GenericTree::new("root", "$sharenote");
        assert!(booths_for(&tree, "42").is_empty());
    }

    #[test]
    fn a_direction_that_will_not_decode_is_counted_rather_than_fatal() {
        let mut tree = GenericTree::new("root", "$sharenote");
        let (pull, assets) = fold(
            &mut tree,
            vec![("P1".into(), 1, b"not a container".to_vec())],
            &Default::default(),
        );
        assert_eq!(pull.directions, 1);
        assert_eq!(pull.units, 0);
        assert!(assets.is_empty());
        assert_eq!(
            pull.unsupported,
            vec!["読み取れない Direction x1".to_string()]
        );
    }
}

#[cfg(test)]
mod post_tests {
    use super::*;
    use tokio::sync::mpsc;

    /// A connection with nobody on the other end but the test itself.
    fn wired() -> (socket::Connection, mpsc::Receiver<socket::Command>) {
        let (tx, rx) = mpsc::channel(64);
        (socket::Connection { commands: tx }, rx)
    }

    fn batch(name: &str, frames: usize) -> Batch {
        Batch {
            frames: (0..frames)
                .map(|i| ("P1_[layer-forUser]_9".to_string(), vec![i as u8]))
                .collect(),
            record: Record::Stroke {
                stroke_id: name.into(),
                element_id: format!("el {name}"),
                layer_id: "P1_[layer-forUser]_9".into(),
            },
        }
    }

    fn stroke_ids(records: &[Record]) -> Vec<&str> {
        records
            .iter()
            .filter_map(|r| match r {
                Record::Stroke { stroke_id, .. } => Some(stroke_id.as_str()),
                _ => None,
            })
            .collect()
    }

    const SOON: Duration = Duration::from_millis(500);

    /// The relay answering, a moment after the posts have gone out.
    fn answer_with(tally: &AckTally, answers: Vec<bool>) {
        let tally = Arc::clone(tally);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tally.lock().unwrap().extend(answers);
        });
    }

    #[tokio::test]
    async fn only_what_the_relay_answered_is_reported_as_sent() {
        // The whole point of the exercise: a post nobody confirmed must not be
        // written into the ledger, because the ledger is what stops a stroke
        // ever being offered again.
        let (connection, _rx) = wired();
        let tally: AckTally = Default::default();
        answer_with(&tally, vec![true, true]);
        let batches = [batch("a", 1), batch("b", 1), batch("c", 1)];
        let confirmed = post_batches_within(&connection, &tally, &batches, SOON).await;
        assert_eq!(stroke_ids(&confirmed), vec!["a", "b"], "c went unanswered");
    }

    #[tokio::test]
    async fn a_post_the_relay_refused_is_not_reported_as_sent() {
        let (connection, _rx) = wired();
        let tally: AckTally = Default::default();
        answer_with(&tally, vec![true, false, true]);
        let batches = [batch("a", 1), batch("b", 1), batch("c", 1)];
        let confirmed = post_batches_within(&connection, &tally, &batches, SOON).await;
        assert_eq!(stroke_ids(&confirmed), vec!["a", "c"], "only b was refused");
    }

    #[tokio::test]
    async fn both_halves_of_a_rasterised_unit_have_to_land() {
        // A picture is two frames — its bytes, then the unit that places them.
        // Recording it on the strength of the first would leave the room
        // showing nothing at all.
        let (connection, _rx) = wired();
        let tally: AckTally = Default::default();
        answer_with(&tally, vec![true]);
        let batches = [batch("picture", 2)];
        let confirmed = post_batches_within(&connection, &tally, &batches, SOON).await;
        assert!(confirmed.is_empty());
    }

    #[tokio::test]
    async fn another_posters_answers_are_not_mistaken_for_ours() {
        // A watch's connection is shared and its tally keeps growing, so what
        // was already there when this call started is somebody else's.
        let (connection, _rx) = wired();
        let tally: AckTally = Arc::new(Mutex::new(vec![false, false, true]));
        answer_with(&tally, vec![true]);
        let batches = [batch("a", 1)];
        let confirmed = post_batches_within(&connection, &tally, &batches, SOON).await;
        assert_eq!(stroke_ids(&confirmed), vec!["a"]);
    }

    #[tokio::test]
    async fn and_neither_are_the_refusals_that_were_already_there() {
        let (connection, _rx) = wired();
        let tally: AckTally = Arc::new(Mutex::new(vec![true, true]));
        answer_with(&tally, vec![false]);
        let batches = [batch("a", 1)];
        let confirmed = post_batches_within(&connection, &tally, &batches, SOON).await;
        assert!(confirmed.is_empty(), "ours is the one that was refused");
    }

    #[tokio::test]
    async fn a_socket_that_is_gone_swallows_nothing() {
        // A watch whose room has ended still takes frames without complaining
        // right up until its writer task notices. Reporting those as sent is
        // how a note's writing came to be missing from the class for good.
        let (connection, rx) = wired();
        drop(rx);
        let batches = [batch("a", 1), batch("b", 1)];
        let confirmed =
            post_batches_within(&connection, &Default::default(), &batches, SOON).await;
        assert!(confirmed.is_empty());
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use serde_json::json;

    fn author() -> send::Author {
        send::Author {
            user_id: "1".into(),
            name: "田中 博悠".into(),
            company_id: "2".into(),
            room_id: "3".into(),
            room_user_id: "4".into(),
        }
    }

    fn pending(id: &str) -> send::Pending {
        send::Pending {
            stroke_id: id.into(),
            layer_id: "P1_[layer-forUser]_9".into(),
            stroke: json!({
                "id": id,
                "points": { "$points": [1.0, 2.0, 0.5, 0.0, 3.0, 4.0, 0.5, 8.0] },
                "color": "#000000",
                "width": 2.0,
            }),
        }
    }

    #[test]
    fn a_stroke_is_one_batch_keyed_by_the_notes_own_id() {
        let batches = build_posts(&[pending("local-1")], &[], &author()).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].frames.len(), 1);
        match &batches[0].record {
            Record::Stroke {
                stroke_id,
                element_id,
                layer_id,
            } => {
                // The ledger is keyed by the note's id, not the room's, so a
                // save that rewrites the stroke can still find the row.
                assert_eq!(stroke_id, "local-1");
                assert!(element_id.contains(' '), "{element_id}");
                assert_eq!(layer_id, "P1_[layer-forUser]_9");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_erasure_carries_the_row_to_drop() {
        let removal = send::Ledger {
            stroke_id: "local-1".into(),
            element_id: "el 1".into(),
            layer_id: "P1_[layer-forUser]_9".into(),
        };
        let batches = build_posts(&[], &[removal], &author()).unwrap();
        assert_eq!(
            batches[0].record,
            Record::Erased {
                stroke_id: "local-1".into()
            }
        );
    }

    #[test]
    fn a_rasterised_unit_is_one_batch_of_two_frames() {
        // Its bytes and the unit that places them travel together, so neither
        // can be recorded without the other.
        let unit = send::PendingUnit::Image {
            unit_id: "shape-1".into(),
            layer_id: "P1_[layer-forUser]_9".into(),
            ticket: "tkt-1".into(),
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let batches = build_unit_posts(&[unit], &author()).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].frames.len(), 2);
        match &batches[0].record {
            Record::Unit { unit_id, .. } => assert_eq!(unit_id, "shape-1"),
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;

    #[tokio::test]
    async fn the_room_saying_who_we_are_is_what_is_waited_for() {
        let slot: LoginSlot = Default::default();
        note_login(
            &slot,
            &CollaboEvent::LoggedIn {
                ok: true,
                message: None,
                room_type: None,
                user_id: Some("1786500056276".into()),
                roles: Vec::new(),
            },
        );
        assert_eq!(
            await_login(&slot).await.unwrap(),
            Some("1786500056276".into())
        );
    }

    #[tokio::test]
    async fn a_refused_login_is_an_error_rather_than_a_silent_success() {
        // It used to be neither: nothing read the answer, so the posts that
        // followed went to a connection the relay did not consider to be in
        // the room, and the send reported success anyway.
        let slot: LoginSlot = Default::default();
        note_login(
            &slot,
            &CollaboEvent::LoggedIn {
                ok: false,
                message: Some("bad user".into()),
                room_type: None,
                user_id: None,
                roles: Vec::new(),
            },
        );
        let err = await_login(&slot).await.unwrap_err().to_string();
        assert!(err.contains("bad user"), "{err}");
    }
}

#[cfg(test)]
mod survey_tests {
    use super::*;
    use crate::atdoc::{write_document, DocumentMeta};
    use crate::model::{GenericModel, GenericTree};
    use serde_json::{json, Value};

    /// A direction as the wire carries one.
    fn direction(build: impl FnOnce(&mut GenericTree) -> Value) -> (String, i64, Vec<u8>) {
        let mut tree = GenericTree::new("direction", "direction");
        let data = build(&mut tree);
        if let Value::Object(props) = &mut tree.models.get_mut("direction").unwrap().props {
            props.insert("data".into(), data);
            props.insert("target".into(), json!("b_[unit]_draw"));
        }
        (
            "b".into(),
            1,
            write_document(&tree, &DocumentMeta::default()).unwrap(),
        )
    }

    fn model(id: &str, parent: Option<&str>, kind: &str, props: Value) -> GenericModel {
        let mut m = GenericModel {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            model_type: kind.into(),
            props,
            children: Vec::new(),
        };
        if parent == Some("direction") {
            if let Value::Object(p) = &mut m.props {
                crate::atdoc::mark_detached(p);
            }
        }
        m
    }

    fn add(element_id: &str) -> (String, i64, Vec<u8>) {
        direction(|tree| {
            tree.insert(model("d", Some("direction"), "D", json!({ "T": 0 })));
            tree.insert(model(
                "i0",
                Some("d"),
                "i",
                json!({ "i": element_id, "m": { "$ref": "e" } }),
            ));
            tree.insert(model(
                "e",
                Some("direction"),
                "E",
                json!({ "I": element_id, "T": 1, "P": { "$points": [1.0, 2.0, 3.0, 4.0] },
                        "BX": 1.0, "BY": 2.0, "BW": 2.0, "BH": 2.0 }),
            ));
            json!({ "$ref": "d" })
        })
    }

    fn remove(element_id: &str) -> (String, i64, Vec<u8>) {
        direction(|tree| {
            tree.insert(model("d", Some("direction"), "D", json!({ "T": 0 })));
            tree.insert(model(
                "i0",
                Some("d"),
                "i",
                json!({ "i": element_id, "t": 1 }),
            ));
            json!({ "$ref": "d" })
        })
    }

    #[test]
    fn what_the_room_holds_is_what_was_added_and_not_taken_back() {
        let state = survey(&[add("a"), add("b"), remove("a")]);
        assert!(state.holds("b"));
        assert!(!state.holds("a"), "erased");
        assert!(!state.holds("c"), "never there");
    }

    #[test]
    fn an_element_it_never_saw_is_told_apart_from_one_it_dropped() {
        // The repair differs: a post that never landed goes out again, and an
        // element someone erased must not.
        let state = survey(&[add("a"), remove("a")]);
        assert!(state.added.contains("a"), "the room saw this one");
        assert!(!state.added.contains("z"), "and never saw this one");
    }

    #[test]
    fn an_empty_room_holds_nothing() {
        let state = survey(&[]);
        assert!(state.added.is_empty());
        assert!(!state.holds("a"));
    }
}
