//! Output fan-out and reconnect snapshots for one PTY.

use serde::Serialize;
use std::{collections::HashMap, sync::Mutex, time::{Duration, Instant}};

pub type SubscriberId = u64;
const MAX_SUBSCRIBERS: usize = 32;

pub trait PtySink: Send + 'static {
    /// Bound before the first output byte; a responder must never look up a replacement by id.
    fn bind_input(&mut self, _input: crate::pty::PtyInput) {}
    /// `false` = the subscriber is gone; it is removed from the stream.
    fn data(&mut self, bytes: &[u8]) -> bool;
    fn exit(&mut self);
    /// #62: the PTY took a new size (another client's, maybe). In order with `data`: the bytes after
    /// it are drawn for that size, so the view resizes its terminal before painting them.
    fn resized(&mut self, _cols: u16, _rows: u16) {}
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Completed lines above the visible viewport, oldest first.
    pub scrollback: Vec<u8>,
    /// Exact ANSI redraw of the visible viewport and terminal modes.
    pub ansi: Vec<u8>,
    pub cursor: (u16, u16),
    pub rows: u16,
    pub cols: u16,
    pub alternate_screen: bool,
}

impl Snapshot {
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.scrollback.len() + self.ansi.len());
        bytes.extend_from_slice(&self.scrollback);
        bytes.extend_from_slice(&self.ansi);
        bytes
    }
}

struct State {
    terminal: vt100::Parser,
    subs: HashMap<SubscriberId, Box<dyn PtySink>>,
    next_subscriber: SubscriberId,
    closed: bool,
    last_output: Option<Instant>,
    scrollback: usize,
    pending_scrollback: Option<usize>,
}

pub struct PtyStream {
    state: Mutex<State>,
}

impl Default for PtyStream {
    fn default() -> Self {
        Self::new(24, 80, 2_000)
    }
}

impl PtyStream {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            state: Mutex::new(State {
                terminal: vt100::Parser::new(rows, cols, scrollback),
                subs: HashMap::new(),
                next_subscriber: 1,
                closed: false,
                last_output: None,
                scrollback,
                pending_scrollback: None,
            }),
        }
    }

    pub fn subscribe(&self, sink: impl PtySink) -> Result<SubscriberId, String> {
        self.attach(sink).map(|(id, _)| id)
    }

    /// Snapshot creation and subscription share one lock with output processing. Bytes arriving at
    /// the boundary therefore occur either in the snapshot or in the live stream, never both.
    pub fn attach(&self, mut sink: impl PtySink) -> Result<(SubscriberId, Snapshot), String> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            let snapshot = snapshot(&state.terminal);
            sink.exit();
            return Ok((0, snapshot));
        }
        if state.subs.len() >= MAX_SUBSCRIBERS {
            return Err("too many PTY subscribers".into());
        }
        // A retired reader may still complete parser state; cap its replay without rebuilding it.
        let snapshot = snapshot_limited(&state.terminal, state.pending_scrollback.unwrap_or(usize::MAX));
        let id = state.next_subscriber;
        state.next_subscriber = state.next_subscriber.wrapping_add(1).max(1);
        state.subs.insert(id, Box::new(sink));
        Ok((id, snapshot))
    }

    pub fn detach(&self, subscriber: SubscriberId) -> bool {
        self.state
            .lock()
            .unwrap()
            .subs
            .remove(&subscriber)
            .is_some()
    }

    /// Keep a smaller finished replay, waiting for EOF so split CSI/UTF-8 sequences can complete.
    pub(crate) fn limit_scrollback(&self, lines: usize) {
        let mut state = self.state.lock().unwrap();
        if state.scrollback <= lines {
            return;
        }
        if !state.closed {
            state.pending_scrollback = Some(state.pending_scrollback.unwrap_or(lines).min(lines));
            return;
        }
        limit_scrollback(&mut state, lines);
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let mut state = self.state.lock().unwrap();
        let screen = state.terminal.screen_mut();
        let old_rows = screen.size().0;
        if rows == old_rows || screen.alternate_screen() {
            screen.set_size(rows, cols);
        } else {
            screen.set_size(old_rows, cols);
            let scrollback = state.scrollback;
            state.terminal = resized(state.terminal.screen(), rows, scrollback);
        }
        for sink in state.subs.values_mut() {
            sink.resized(cols, rows);
        }
    }

    pub fn publish(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.terminal.process(bytes);
        state.last_output = Some(Instant::now());
        state.subs.retain(|_, sink| sink.data(bytes));
    }

    /// FIX-1: how long the output has been still, once the screen shows something. A starting TUI
    /// first sends only terminal queries (blank screen) and can then think for seconds; a drawn
    /// screen that stops changing is waiting on a key.
    pub fn settled_for(&self) -> Option<Duration> {
        let state = self.state.lock().unwrap();
        let at = state.last_output.filter(|_| !state.closed)?;
        let drawn = state.terminal.screen().contents().chars().any(|c| !c.is_whitespace());
        drawn.then(|| at.elapsed())
    }

    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// S5-B: the visible screen as plain text (the waiting question shown in the chat card).
    pub fn contents(&self) -> String {
        self.state.lock().unwrap().terminal.screen().contents()
    }

    /// The child's output ended; every subscriber hears it once.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        state.closed = true;
        if let Some(lines) = state.pending_scrollback.take() {
            limit_scrollback(&mut state, lines);
        }
        for (_, mut sink) in state.subs.drain() {
            sink.exit();
        }
    }
}

/// Snapshot reconstruction is safe only once no more output can complete parser state.
fn limit_scrollback(state: &mut State, lines: usize) {
    let snapshot = snapshot(&state.terminal);
    let mut terminal = vt100::Parser::new(snapshot.rows, snapshot.cols, lines);
    terminal.process(&snapshot.bytes());
    state.terminal = terminal;
    state.scrollback = lines;
}

/// #35: the screen with `rows` rows, the way conhost resizes it before ConPTY repaints the viewport
/// from the top. A shrink pushes the rows above the last written one into scrollback (blank rows at
/// the bottom go first) and a grow pulls them back; vt100 instead drops the rows below a shrinking
/// screen, so the repaint overwrote the top rows and the snapshot lost a block mid-history. Colors are
/// dropped like in the snapshot's history; the repaint brings the viewport's back.
fn resized(old: &vt100::Screen, rows: u16, scrollback: usize) -> vt100::Parser {
    let (old_rows, cols) = old.size();
    let (row, col) = old.cursor_position();
    let written = old
        .rows(0, cols)
        .enumerate()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(i, _)| i as u16 + 1)
        .last()
        .unwrap_or(0);
    let kept = old_rows.min(rows.max(written).max(row + 1));

    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut history = old.clone();
    history.set_scrollback(usize::MAX);
    let history_rows = history.scrollback();
    for offset in (1..=history_rows).rev() {
        history.set_scrollback(offset);
        lines.push((history.rows(0, cols).next().unwrap_or_default(), history.row_wrapped(0)));
    }
    for (i, text) in old.rows(0, cols).take(usize::from(kept)).enumerate() {
        lines.push((text, old.row_wrapped(i as u16)));
    }

    let mut parser = vt100::Parser::new(rows, cols, scrollback);
    let mut wrapped = true;
    for (text, wraps) in &lines {
        if !wrapped {
            parser.process(b"\r\n");
        }
        parser.process(text.as_bytes());
        wrapped = *wraps;
    }
    let row = if kept >= rows {
        row.saturating_sub(kept - rows)
    } else {
        row + u16::try_from(history_rows).unwrap_or(u16::MAX).min(rows - kept)
    };
    let mut tail = old.input_mode_formatted();
    tail.extend_from_slice(&old.attributes_formatted());
    tail.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    parser.process(&tail);
    parser
}

fn snapshot(parser: &vt100::Parser) -> Snapshot {
    snapshot_limited(parser, usize::MAX)
}

fn snapshot_limited(parser: &vt100::Parser, lines: usize) -> Snapshot {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let alternate_screen = screen.alternate_screen();
    let mut scrollback = Vec::new();

    if !alternate_screen {
        let mut history = screen.clone();
        history.set_scrollback(usize::MAX);
        let count = history.scrollback().min(lines);
        for offset in (1..=count).rev() {
            history.set_scrollback(offset);
            scrollback
                .extend_from_slice(history.rows(0, cols).next().unwrap_or_default().as_bytes());
            if !history.row_wrapped(0) {
                scrollback.extend_from_slice(b"\r\n");
            }
        }
        // #35: the redraw below starts by erasing the screen, and the last rows of history are still on
        // it — push them up into the viewer's scrollback first, or they vanish from the middle.
        if count > 0 {
            if !scrollback.ends_with(b"\r\n") {
                scrollback.extend_from_slice(b"\r\n");
            }
            scrollback.extend_from_slice(&b"\r\n".repeat(usize::from(rows) - 1));
        }
    }

    let mut ansi = Vec::new();
    if alternate_screen {
        ansi.extend_from_slice(b"\x1b[?1049h");
    }
    ansi.extend_from_slice(&screen.state_formatted());

    Snapshot {
        scrollback,
        ansi,
        cursor: screen.cursor_position(),
        rows,
        cols,
        alternate_screen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<(Vec<u8>, u32)>>, Option<usize>);
    impl PtySink for Rec {
        fn data(&mut self, bytes: &[u8]) -> bool {
            let mut g = self.0.lock().unwrap();
            if self.1.is_some_and(|max| g.0.len() >= max) {
                return false;
            }
            g.0.extend_from_slice(bytes);
            true
        }
        fn exit(&mut self) {
            self.0.lock().unwrap().1 += 1;
        }
    }

    #[test]
    fn settles_only_once_something_is_drawn() {
        let s = PtyStream::new(4, 20, 20);
        assert_eq!(s.settled_for(), None, "no output");
        s.publish(b"[1t[c[?1004h]0;claude\\");
        assert_eq!(s.settled_for(), None, "startup queries draw nothing");
        s.publish(b"[2;3H> 1. Yes");
        std::thread::sleep(Duration::from_millis(30));
        assert!(s.settled_for().unwrap() >= Duration::from_millis(30));
        s.publish(b"[?25l");
        assert!(s.settled_for().unwrap() < Duration::from_millis(30), "any output restarts the clock");
        s.close();
        assert_eq!(s.settled_for(), None, "exited");
    }

    #[test]
    fn fans_out_and_drops_gone_subscribers() {
        let s = PtyStream::default();
        let (a, b) = (Rec::default(), Rec(Arc::default(), Some(2)));
        s.subscribe(a.clone()).unwrap();
        s.subscribe(b.clone()).unwrap();
        s.publish(b"ab");
        s.publish(b"cd");
        s.publish(b"ef");
        s.close();
        s.close();
        assert_eq!(*a.0.lock().unwrap(), (b"abcdef".to_vec(), 1));
        assert_eq!(*b.0.lock().unwrap(), (b"ab".to_vec(), 0));
    }

    #[test]
    fn snapshot_recreates_color_cursor_and_alternate_screen() {
        let s = PtyStream::new(4, 20, 20);
        s.publish(b"normal\r\n\x1b[31mred\x1b[0m\x1b[2;3H");
        let (_, normal) = s.attach(Rec::default()).unwrap();
        assert_eq!((normal.rows, normal.cols, normal.cursor), (4, 20, (1, 2)));

        let mut replay = vt100::Parser::new(normal.rows, normal.cols, 20);
        replay.process(&normal.bytes());
        assert_eq!(
            replay.screen().cell(1, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        assert_eq!(replay.screen().cursor_position(), (1, 2));

        s.publish(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[34mTUI\x1b[0m\x1b[3;5H");
        let (_, alt) = s.attach(Rec::default()).unwrap();
        assert!(alt.alternate_screen);
        let mut replay = vt100::Parser::new(alt.rows, alt.cols, 20);
        replay.process(&alt.bytes());
        assert!(replay.screen().alternate_screen());
        assert_eq!(replay.screen().contents().lines().next(), Some("TUI"));
        assert_eq!(replay.screen().cursor_position(), (2, 4));
    }

    #[test]
    fn attach_boundary_has_no_loss_or_duplicate_bytes() {
        for n in 0..100 {
            let s = Arc::new(PtyStream::new(4, 80, 20));
            let rec = Rec::default();
            let out = rec.0.clone();
            let marker = format!("marker-{n}");
            let producer = {
                let s = s.clone();
                let marker = marker.clone();
                std::thread::spawn(move || s.publish(marker.as_bytes()))
            };
            let (_, snapshot) = s.attach(rec).unwrap();
            producer.join().unwrap();
            let snap = String::from_utf8_lossy(&snapshot.ansi);
            let live = String::from_utf8_lossy(&out.lock().unwrap().0).into_owned();
            assert_eq!(
                snap.matches(&marker).count() + live.matches(&marker).count(),
                1,
                "{n}: {snap:?} + {live:?}"
            );
        }
    }

    #[test]
    fn detach_removes_only_the_named_subscriber() {
        let s = PtyStream::new(2, 20, 5);
        let (a, b) = (Rec::default(), Rec::default());
        let (aid, _) = s.attach(a.clone()).unwrap();
        let (_bid, _) = s.attach(b.clone()).unwrap();
        assert!(s.detach(aid));
        s.publish(b"x");
        assert!(a.0.lock().unwrap().0.is_empty());
        assert_eq!(b.0.lock().unwrap().0, b"x");
    }

    #[test]
    fn snapshot_includes_bounded_scrollback() {
        let s = PtyStream::new(2, 20, 3);
        s.publish(b"one\r\ntwo\r\nthree\r\nfour");
        let (_, snapshot) = s.attach(Rec::default()).unwrap();
        let history = String::from_utf8(snapshot.scrollback).unwrap();
        assert!(
            history.contains("one") && history.contains("two"),
            "{history:?}"
        );
        assert!(
            !history.contains("four"),
            "visible viewport leaked into history: {history:?}"
        );
    }

    #[test]
    fn closed_stream_replays_final_output_and_exits_without_a_subscription() {
        let s = PtyStream::new(2, 40, 3);
        s.publish(b"history\r\nRX:FAST_FINAL_OUTPUT");
        s.close();
        for _ in 0..MAX_SUBSCRIBERS + 1 {
            let rec = Rec::default();
            let (subscriber, snapshot) = s.attach(rec.clone()).unwrap();
            assert_eq!(subscriber, 0, "replay does not hold a subscriber");
            assert!(String::from_utf8_lossy(&snapshot.bytes()).contains("RX:FAST_FINAL_OUTPUT"));
            assert_eq!(*rec.0.lock().unwrap(), (Vec::new(), 1));
            assert!(!s.detach(subscriber));
        }
        s.close();
    }

    #[test]
    fn limiting_finished_history_preserves_the_viewport_and_terminal_modes() {
        for alternate in [false, true] {
            let s = PtyStream::new(4, 20, 10_000);
            let history: String = (0..3_000).map(|i| format!("line-{i}\r\n")).collect();
            s.publish(history.as_bytes());
            if alternate { s.publish(b"\x1b[?1049h\x1b[2J\x1b[H"); }
            s.publish(b"\x1b[31mFINAL\x1b[?25l\x1b[2;3H");
            s.close();
            let before = s.attach(Rec::default()).unwrap().1;
            s.limit_scrollback(2_000);
            let after = s.attach(Rec::default()).unwrap().1;
            assert_eq!(after.ansi, before.ansi);
            assert_eq!(after.cursor, before.cursor);
            assert_eq!(after.alternate_screen, alternate);
            assert_eq!(s.state.lock().unwrap().scrollback, 2_000);
        }
    }

    #[test]
    fn scrollback_limit_waits_for_eof_and_needs_no_attach() {
        let s = PtyStream::new(2, 40, 10_000);
        let history: String = (0..3_005).map(|i| format!("line-{i:04}\r\n")).collect();
        s.publish(history.as_bytes());
        s.limit_scrollback(2_000);
        assert_eq!(s.state.lock().unwrap().scrollback, 10_000, "output is still open");
        s.limit_scrollback(2_500);
        s.publish(b"FINAL_TAIL");
        s.close();
        let state = s.state.lock().unwrap();
        assert_eq!(state.scrollback, 2_000, "EOF applies the tightest requested limit");
        let snapshot = snapshot(&state.terminal);
        let history = String::from_utf8(snapshot.scrollback).unwrap();
        assert_eq!(history.lines().filter(|line| line.starts_with("line-")).count(), 2_000);
        assert!(!history.contains("line-0000"));
        assert!(String::from_utf8_lossy(&snapshot.ansi).contains("FINAL_TAIL"));
    }

    #[test]
    fn subscriber_memory_is_bounded() {
        let s = PtyStream::new(2, 20, 3);
        for _ in 0..MAX_SUBSCRIBERS {
            s.attach(Rec::default()).unwrap();
        }
        assert!(s.attach(Rec::default()).is_err());
    }

    /// Every non-empty line a fresh viewer holds after painting the snapshot, history first.
    fn painted(snapshot: &Snapshot) -> Vec<String> {
        let mut viewer = vt100::Parser::new(snapshot.rows, snapshot.cols, 1_000);
        viewer.process(&snapshot.bytes());
        let mut screen = viewer.screen().clone();
        screen.set_scrollback(usize::MAX);
        let mut lines = Vec::new();
        for offset in (1..=screen.scrollback()).rev() {
            screen.set_scrollback(offset);
            lines.extend(screen.rows(0, snapshot.cols).next());
        }
        screen.set_scrollback(0);
        lines.extend(screen.rows(0, snapshot.cols));
        lines.retain(|line| !line.is_empty());
        lines
    }

    /// ConPTY answers a resize by repainting its whole viewport from the top.
    fn repaint(lines: &[&str]) -> Vec<u8> {
        let rows: Vec<String> = lines.iter().map(|line| format!("\x1b[2K{line}")).collect();
        format!("\x1b[H{}", rows.join("\r\n")).into_bytes()
    }

    #[test]
    fn painted_snapshot_keeps_the_history_right_above_the_screen() {
        let s = PtyStream::new(4, 20, 100);
        for i in 1..=10 {
            s.publish(format!("line{i}\r\n").as_bytes());
        }
        let (_, snapshot) = s.attach(Rec::default()).unwrap();
        let want: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        assert_eq!(painted(&snapshot), want);
    }

    #[test]
    fn shrink_and_grow_under_conpty_lose_and_repeat_nothing() {
        let all = ["a1", "a2", "a3", "a4", "a5", "> input", "status"];
        let s = PtyStream::new(6, 20, 100);
        // a1 scrolls into history; the cursor sits on the input row with the status row below it.
        s.publish(b"a1\r\na2\r\na3\r\na4\r\na5\r\n> input\r\nstatus\x1b[1A\x1b[3G");

        s.resize(3, 20);
        s.publish(&repaint(&all[4..]));
        let (_, snapshot) = s.attach(Rec::default()).unwrap();
        assert_eq!(painted(&snapshot), all, "shrunk");

        s.resize(6, 20);
        s.publish(&repaint(&all[1..]));
        let (_, snapshot) = s.attach(Rec::default()).unwrap();
        assert_eq!(painted(&snapshot), all, "grown back");
    }

    #[test]
    fn shrinking_a_short_screen_drops_only_blank_rows() {
        let s = PtyStream::new(6, 20, 100);
        s.publish(b"a1\r\n> input");
        s.resize(3, 20);
        s.publish(&repaint(&["a1", "> input", ""]));
        let (_, snapshot) = s.attach(Rec::default()).unwrap();
        assert_eq!(painted(&snapshot), ["a1", "> input"]);
    }
}
