//! `trace.json`: the Chrome trace events `tsc --generateTrace` writes,
//! read as analyze-trace's `parse-trace-file.ts` reads them.
//!
//! The file is streamed: a big project's trace runs to gigabytes, and only
//! spans at least `skipMillis` long are kept. Each element of the array is
//! cut out by [`Elements`] and parsed on its own, so an event that can't
//! be read (a number out of range, arguments nested too deep, a field of
//! the wrong type) is skipped without losing the rest of the trace.
//! Arguments are parsed only for the events that need them. Begin (`B`)
//! and end (`E`) events are paired per process and thread; complete (`X`)
//! events carry their duration. A trace cut short, as when `tsc` crashed,
//! keeps every event before the cut, and the spans still open there run
//! to the end of the trace.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

use serde::{
    Deserialize,
    de::{self, Deserializer, Visitor},
};
use serde_json::{Map, Value, value::RawValue};
use tracing::{debug, warn};

use crate::paths::slashes;

/// One trace event: a span's start or end, or a whole span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub name: Box<str>,
    /// `check`, `checkTypes`, `emit`, `program`, ….
    pub cat: Option<Box<str>>,
    /// Microseconds since `tsc` started.
    pub ts: f64,
    /// The process and thread, which nest spans separately, numbered in
    /// the order first seen.
    pub stack: u32,
    pub args: Option<Map<String, Value>>,
}

impl Event {
    pub(crate) fn arg(&self, key: &str) -> Option<&Value> {
        self.args.as_ref()?.get(key)
    }

    pub(crate) fn arg_str(&self, key: &str) -> Option<&str> {
        self.arg(key)?.as_str()
    }

    /// A numeric argument, as JavaScript's truthiness reads it: `0` and
    /// anything that isn't a number are `None`.
    pub(crate) fn arg_u64(&self, key: &str) -> Option<u64> {
        self.arg(key)?.as_u64().filter(|n| *n != 0)
    }
}

/// A span of time: an event that began and ended, or one still open when
/// the trace ended.
#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub event: Event,
    pub start: f64,
    pub end: f64,
}

/// What a trace holds, as analyze-trace's `parse` returns it.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub min_time: f64,
    pub max_time: f64,
    /// Spans at least the minimum duration long, in the order they ended.
    pub spans: Vec<Span>,
    /// Begin events with no end, innermost of each stack last.
    pub unclosed: Vec<Event>,
    /// How many end events had no begin.
    pub unmatched_ends: usize,
    /// Each package a loaded or parsed file is in, with each directory it
    /// was loaded from, in the order first seen.
    pub packages: Vec<(String, Vec<String>)>,
    /// How many files the trace checked.
    pub checked_files: usize,
    /// Some of the files the trace parsed or checked outside
    /// `node_modules`, as it names them: enough to tell where it was
    /// recorded.
    pub sample_files: Vec<String>,
    /// The tsconfig the program was built from, as `createProgram` gives
    /// it.
    pub config_file: Option<String>,
}

/// How many files [`Parsed::sample_files`] holds at most.
const SAMPLE_FILES: usize = 256;

/// Reads the trace at `path`, keeping spans at least `min_duration`
/// microseconds long.
///
/// # Errors
/// Fails when the file can't be opened or isn't a trace.
pub(crate) fn parse_file(path: &Path, min_duration: f64) -> std::io::Result<Parsed> {
    let file = File::open(path)?;
    parse_reader(BufReader::with_capacity(1 << 16, file), min_duration)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// As [`parse_file`], from `reader`.
pub(crate) fn parse_reader(reader: impl BufRead, min_duration: f64) -> Result<Parsed, String> {
    let mut state = State::new(min_duration);
    let mut elements = Elements::new(reader);
    let end = loop {
        match elements.next() {
            Ok(Some(element)) => state.add(element),
            Ok(None) => break None,
            Err(e) => break Some(e),
        }
    };
    if state.skipped > 0 {
        debug!("Skipped {} trace events that couldn't be read", state.skipped);
    }
    match end {
        None if state.items > 0 && state.events == 0 => {
            return Err("not a trace (it holds no events)".into());
        }
        None => {}
        Some(e) if state.events == 0 => return Err(format!("not a trace ({e})")),
        Some(e) => warn!("The trace is cut short or malformed after {} events: {e}", state.events),
    }
    Ok(state.finish())
}

/// A process or thread id; `1` when absent, as analyze-trace's default.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Id {
    Number(u64),
    Text(Box<str>),
}

impl Id {
    /// The id `raw` gives, as JavaScript prints it into the stack key.
    fn of(raw: Option<&RawValue>) -> Self {
        let text = raw.map_or("null", RawValue::get);
        if text == "null" {
            return Self::Number(1);
        }
        if let Ok(n) = text.parse() {
            return Self::Number(n);
        }
        match serde_json::from_str::<Cow<'_, str>>(text) {
            Ok(s) => s.parse().map_or_else(|_| Self::Text(s.into()), Self::Number),
            Err(_) => Self::Text(text.into()),
        }
    }
}

/// A trace event as written, with its arguments left unparsed.
#[derive(Deserialize)]
struct RawEvent<'a> {
    #[serde(borrow, default)]
    ph: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    cat: Option<Cow<'a, str>>,
    #[serde(default)]
    ts: Number,
    #[serde(default)]
    dur: Number,
    #[serde(borrow, default)]
    pid: Option<&'a RawValue>,
    #[serde(borrow, default)]
    tid: Option<&'a RawValue>,
    #[serde(borrow, default)]
    args: Option<&'a RawValue>,
}

/// The file arguments of the events that load and check files.
#[derive(Deserialize)]
struct FileArgs<'a> {
    #[serde(borrow, default, rename = "fileName")]
    file_name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    path: Option<Cow<'a, str>>,
}

/// The arguments of `createProgram`.
#[derive(Deserialize)]
struct ProgramArgs<'a> {
    #[serde(borrow, default, rename = "configFilePath")]
    config_file_path: Option<Cow<'a, str>>,
}

/// `+value` in JavaScript, for a number or a numeric string; `None` when
/// absent or not a finite number.
#[derive(Debug, Default, Clone, Copy)]
struct Number(Option<f64>);

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Number;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Number, E> {
                Ok(Number(Some(v).filter(|n| n.is_finite())))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Number, E> {
                Ok(Number(Some(v as f64)))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Number, E> {
                Ok(Number(Some(v as f64)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Number, E> {
                Ok(Number(v.trim().parse().ok().filter(|n: &f64| n.is_finite())))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Number, E> {
                Ok(Number(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Number, E> {
                Ok(Number(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// A begin event waiting for its end.
struct Begin {
    name: Box<str>,
    cat: Option<Box<str>>,
    ts: f64,
    args: Option<Box<RawValue>>,
}

struct State {
    min_duration: f64,
    out: Parsed,
    /// Each process and thread to its stack's number.
    stack_ids: HashMap<(Id, Id), u32>,
    /// The open begin events of each stack, by number.
    stacks: Vec<Vec<Begin>>,
    package_index: HashMap<String, usize>,
    /// Objects read.
    events: usize,
    /// Items read, events or not.
    items: usize,
    /// Objects that couldn't be read as events.
    skipped: usize,
}

impl State {
    fn new(min_duration: f64) -> Self {
        Self {
            min_duration,
            out: Parsed { min_time: f64::INFINITY, max_time: 0.0, ..Parsed::default() },
            stack_ids: HashMap::new(),
            stacks: Vec::new(),
            package_index: HashMap::new(),
            events: 0,
            items: 0,
            skipped: 0,
        }
    }

    fn stack(&mut self, pid: Option<&RawValue>, tid: Option<&RawValue>) -> u32 {
        let key = (Id::of(pid), Id::of(tid));
        if let Some(&id) = self.stack_ids.get(&key) {
            return id;
        }
        let id = u32::try_from(self.stacks.len()).unwrap_or(u32::MAX);
        self.stacks.push(Vec::new());
        self.stack_ids.insert(key, id);
        id
    }

    fn add(&mut self, element: &[u8]) {
        self.items += 1;
        if element.first() != Some(&b'{') {
            return;
        }
        self.events += 1;
        let raw: RawEvent<'_> = match serde_json::from_slice(element) {
            Ok(raw) => raw,
            Err(e) => {
                self.skipped += 1;
                debug!("Skipping a trace event that can't be read: {e}");
                return;
            }
        };
        let ph = raw.ph.as_deref().unwrap_or_default();
        // Metadata and instant events say nothing about time spent.
        if matches!(ph, "M" | "i" | "I") {
            return;
        }
        let Some(ts) = raw.ts.0 else {
            debug!("Skipping a trace event without a time");
            return;
        };
        let name = raw.name.as_deref().unwrap_or_default();
        let stack = self.stack(raw.pid, raw.tid);
        let (start, end, begin_args) = match ph {
            "B" => {
                self.stacks[stack as usize].push(Begin {
                    name: name.into(),
                    cat: raw.cat.as_deref().map(Into::into),
                    ts,
                    args: raw.args.map(RawValue::to_owned),
                });
                return;
            }
            "E" => {
                let open = &mut self.stacks[stack as usize];
                let matches = open
                    .last()
                    .is_some_and(|b| *b.name == *name && b.cat.as_deref() == raw.cat.as_deref());
                if !matches {
                    self.out.unmatched_ends += 1;
                    return;
                }
                let Some(begin) = open.pop() else { return };
                (begin.ts, ts, Some(begin))
            }
            "X" => (ts, ts + raw.dur.0.unwrap_or(0.0).max(0.0), None),
            other => {
                debug!("Skipping a trace event of unknown phase {other:?}");
                return;
            }
        };
        self.out.min_time = self.out.min_time.min(start);
        self.out.max_time = self.out.max_time.max(end);
        let checks_file = name == "checkSourceFile";
        if checks_file {
            self.out.checked_files += 1;
        }
        // Before spans are dropped for being short. analyze-trace reads only
        // `findSourceFile`, which `tsc` writes only when it took long enough
        // to be sampled; `createSourceFile` is written for every file parsed.
        if name == "createProgram"
            && self.out.config_file.is_none()
            && let Some(args) = begin_args.as_ref().and_then(|b| b.args.as_deref()).or(raw.args)
        {
            self.out.config_file = serde_json::from_str::<ProgramArgs<'_>>(args.get())
                .ok()
                .and_then(|a| a.config_file_path)
                .map(Cow::into_owned);
        }
        let loads = matches!(name, "findSourceFile" | "createSourceFile");
        if loads || (checks_file && self.out.sample_files.len() < SAMPLE_FILES) {
            let args = begin_args.as_ref().and_then(|b| b.args.as_deref()).or(raw.args);
            self.read_file_args(name, args);
        }
        if end - start < self.min_duration {
            return;
        }
        let event = match begin_args {
            Some(begin) => {
                let mut args = begin.args.as_deref().and_then(parse_args);
                if let Some(end_args) = raw.args.and_then(parse_args) {
                    args.get_or_insert_with(Map::new).extend(end_args);
                }
                Event { name: begin.name, cat: begin.cat, ts: begin.ts, stack, args }
            }
            None => Event {
                name: name.into(),
                cat: raw.cat.as_deref().map(Into::into),
                ts,
                stack,
                args: raw.args.and_then(parse_args),
            },
        };
        self.out.spans.push(Span { event, start, end });
    }

    /// Records the packages of the file a loading event names, and the
    /// file as a sample of those the trace read.
    fn read_file_args(&mut self, name: &str, args: Option<&RawValue>) {
        let Some(args) = args.and_then(|a| serde_json::from_str::<FileArgs<'_>>(a.get()).ok())
        else {
            return;
        };
        let file = match name {
            "findSourceFile" => args.file_name,
            _ => args.path,
        };
        let Some(file) = file else { return };
        let file = slashes(&file);
        if name != "checkSourceFile" {
            self.record_packages(&file);
        }
        if self.out.sample_files.len() < SAMPLE_FILES
            && !file.contains("/node_modules/")
            && !file.ends_with(".json")
        {
            self.out.sample_files.push(file.into_owned());
        }
    }

    /// Records each `node_modules` package directory on the way to `file`,
    /// as analyze-trace's pattern
    /// `/\/node_modules\/((?:[^@][^/]+)|(?:@[^/]+\/[^/]+))/g` finds them.
    fn record_packages(&mut self, file: &str) {
        for (name, dir) in package_dirs(file) {
            match self.package_index.get(name) {
                Some(&i) => {
                    let dirs = &mut self.out.packages[i].1;
                    if !dirs.iter().any(|d| d == dir) {
                        dirs.push(dir.to_string());
                    }
                }
                None => {
                    self.package_index.insert(name.to_string(), self.out.packages.len());
                    self.out.packages.push((name.to_string(), vec![dir.to_string()]));
                }
            }
        }
    }

    fn finish(mut self) -> Parsed {
        for (stack, open) in self.stacks.into_iter().enumerate() {
            let stack = u32::try_from(stack).unwrap_or(u32::MAX);
            self.out.unclosed.extend(open.into_iter().map(|b| Event {
                args: b.args.as_deref().and_then(parse_args),
                name: b.name,
                cat: b.cat,
                ts: b.ts,
                stack,
            }));
        }
        if self.out.min_time > self.out.max_time {
            self.out.min_time = self.out.max_time;
        }
        self.out.sample_files.sort_unstable();
        self.out.sample_files.dedup();
        self.out
    }
}

/// An event's arguments, when they're an object that can be read.
fn parse_args(raw: &RawValue) -> Option<Map<String, Value>> {
    match serde_json::from_str(raw.get()) {
        Ok(args) => Some(args),
        Err(e) => {
            debug!("Leaving out the arguments of a trace event: {e}");
            None
        }
    }
}

/// Each package directory in `file` with the package's name: for
/// `/r/node_modules/@a/b/node_modules/cc/x.d.ts`, `@a/b` at
/// `/r/node_modules/@a/b` and `cc` at `/r/node_modules/@a/b/node_modules/cc`.
pub(crate) fn package_dirs(file: &str) -> Vec<(&str, &str)> {
    const MARKER: &str = "/node_modules/";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = file[from..].find(MARKER) {
        let at = from + found;
        let name_start = at + MARKER.len();
        let rest = &file[name_start..];
        let name_len = if rest.starts_with('@') {
            // `@scope/name`.
            rest.find('/').filter(|&s| s > 1).and_then(|s| {
                let tail = &rest[s + 1..];
                let n = tail.find('/').unwrap_or(tail.len());
                (n > 0).then_some(s + 1 + n)
            })
        } else {
            // One character that isn't `@`, then at least one that isn't `/`.
            rest.chars().next().and_then(|c| {
                let tail = &rest[c.len_utf8()..];
                let n = tail.find('/').unwrap_or(tail.len());
                (n > 0).then_some(c.len_utf8() + n)
            })
        };
        match name_len {
            Some(len) => {
                let end = name_start + len;
                out.push((&file[name_start..end], &file[..end]));
                from = end;
            }
            None => from = at + 1,
        }
    }
    out
}

/// Cuts the elements of a JSON array, or of the `traceEvents` array of
/// the object form other tools write, out of a stream, one at a time,
/// without parsing them. It tracks only strings and nesting, iteratively,
/// so no element is too deep for it.
struct Elements<R> {
    reader: R,
    scanner: Scanner,
}

/// The state of [`Elements`] between chunks.
struct Scanner {
    /// The element being cut out.
    buf: Vec<u8>,
    /// How deep the scan is: 1 inside the top-level array or object.
    depth: usize,
    /// The depth elements are at: 1, or 2 in `{"traceEvents": [...]}`.
    element_depth: usize,
    /// Whether the top level is an object.
    object: bool,
    in_string: bool,
    escaped: bool,
    /// In the object form: the last key of the top-level object, and
    /// whether the string being read is one.
    key: Vec<u8>,
    reading_key: bool,
    /// Whether the current element is a container, which ends when the
    /// depth comes back.
    container: bool,
    started: bool,
    done: bool,
}

impl<R: BufRead> Elements<R> {
    fn new(reader: R) -> Self {
        Self { reader, scanner: Scanner::new() }
    }

    /// The next element, `None` at the end of the array, or an error when
    /// the input isn't an array or ends in the middle of one.
    fn next(&mut self) -> Result<Option<&[u8]>, String> {
        let s = &mut self.scanner;
        s.buf.clear();
        if s.done {
            return Ok(None);
        }
        loop {
            let chunk = self.reader.fill_buf().map_err(|e| e.to_string())?;
            if chunk.is_empty() {
                s.done = true;
                return Err(if s.started {
                    "it ends before the array closes".into()
                } else {
                    "it's empty".into()
                });
            }
            let (used, complete) = s.scan(chunk)?;
            self.reader.consume(used);
            match complete {
                Some(true) => return Ok(Some(&s.buf)),
                Some(false) => {
                    s.done = true;
                    return Ok(None);
                }
                None => {}
            }
        }
    }
}

impl Scanner {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            depth: 0,
            element_depth: 1,
            object: false,
            in_string: false,
            escaped: false,
            key: Vec::new(),
            reading_key: false,
            container: false,
            started: false,
            done: false,
        }
    }

    /// Scans `chunk`, copying the element's bytes. Returns how much was
    /// used, and `Some(true)` when an element is complete, `Some(false)`
    /// when the array closed.
    fn scan(&mut self, chunk: &[u8]) -> Result<(usize, Option<bool>), String> {
        let mut i = 0;
        // Where the element's bytes in this chunk start.
        let mut from = (!self.buf.is_empty()).then_some(0);
        while i < chunk.len() {
            if self.in_string {
                if self.escaped {
                    // The character after a backslash, even a quote.
                    self.escaped = false;
                    i += 1;
                    continue;
                }
                // Skip to the next quote or backslash.
                let Some(n) = memchr::memchr2(b'"', b'\\', &chunk[i..]) else {
                    if self.reading_key {
                        self.key.extend_from_slice(&chunk[i..]);
                    }
                    i = chunk.len();
                    break;
                };
                if self.reading_key {
                    self.key.extend_from_slice(&chunk[i..i + n]);
                }
                i += n;
                if chunk[i] == b'\\' {
                    self.escaped = true;
                    i += 1;
                    continue;
                }
                self.in_string = false;
                self.reading_key = false;
                if self.ends_element(from.is_some()) {
                    let start = from.unwrap_or(0);
                    self.buf.extend_from_slice(&chunk[start..=i]);
                    return Ok((i + 1, Some(true)));
                }
                i += 1;
                continue;
            }
            let b = chunk[i];
            if !self.started {
                match b {
                    b' ' | b'\t' | b'\n' | b'\r' => {}
                    // A byte order mark.
                    0xef | 0xbb | 0xbf => {}
                    b'[' => {
                        self.started = true;
                        self.depth = 1;
                    }
                    b'{' => {
                        self.started = true;
                        self.object = true;
                        self.depth = 1;
                        self.element_depth = usize::MAX;
                    }
                    _ => return Err("it isn't a JSON array".into()),
                }
                i += 1;
                continue;
            }
            let in_element = from.is_some();
            if self.ends_element(in_element)
                && matches!(b, b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r')
            {
                // The end of a number, `true`, `false` or `null`; what
                // ends it is read again.
                let start = from.unwrap_or(0);
                self.buf.extend_from_slice(&chunk[start..i]);
                return Ok((i, Some(true)));
            }
            if !in_element && self.depth == self.element_depth {
                match b {
                    b' ' | b'\t' | b'\n' | b'\r' | b',' => {}
                    b']' | b'}' => {
                        return Ok((i + 1, Some(false)));
                    }
                    _ => {
                        from = Some(i);
                        self.container = matches!(b, b'{' | b'[');
                        if b == b'"' {
                            self.in_string = true;
                        } else if self.container {
                            self.depth += 1;
                        }
                    }
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => {
                    self.in_string = true;
                    if self.object && self.depth == 1 {
                        self.key.clear();
                        self.reading_key = true;
                    }
                }
                b'{' | b'[' => {
                    if self.object && self.depth == 1 && b == b'[' && self.key == b"traceEvents" {
                        self.element_depth = 2;
                    }
                    self.depth += 1;
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 0 {
                        return Ok((i + 1, Some(false)));
                    }
                    if self.object && self.depth == 1 && self.element_depth == 2 {
                        // The end of `traceEvents`.
                        return Ok((i + 1, Some(false)));
                    }
                    if in_element && self.container && self.depth == self.element_depth {
                        let start = from.unwrap_or(0);
                        self.buf.extend_from_slice(&chunk[start..=i]);
                        return Ok((i + 1, Some(true)));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if let Some(start) = from {
            self.buf.extend_from_slice(&chunk[start..i.min(chunk.len())]);
        }
        Ok((chunk.len(), None))
    }

    /// Whether the element ends where its value does: it's a string or
    /// another scalar, not a container.
    fn ends_element(&self, in_element: bool) -> bool {
        in_element && !self.container && self.depth == self.element_depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, min: f64) -> Parsed {
        parse_reader(text.as_bytes(), min).unwrap()
    }

    fn elements(text: &str, chunk: usize) -> Vec<String> {
        struct Chunks<'a>(&'a [u8], usize);
        impl std::io::Read for Chunks<'_> {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(self.1).min(out.len());
                out[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let reader = BufReader::with_capacity(chunk, Chunks(text.as_bytes(), chunk));
        let mut e = Elements::new(reader);
        let mut out = Vec::new();
        while let Ok(Some(el)) = e.next() {
            out.push(String::from_utf8(el.to_vec()).unwrap());
        }
        out
    }

    #[test]
    fn cuts_elements_whatever_the_chunks() {
        let text =
            r#" [ {"a":"x\"]}","b":[1,{"c":2}]} , 12 ,"s\\",true,[[]],{"d":"\\\""} ] trailing"#;
        let want = [
            r#"{"a":"x\"]}","b":[1,{"c":2}]}"#,
            "12",
            r#""s\\""#,
            "true",
            "[[]]",
            r#"{"d":"\\\""}"#,
        ];
        for chunk in [1, 2, 3, 5, 7, 64, 4096] {
            assert_eq!(elements(text, chunk), want, "chunks of {chunk}");
        }
        let object = r#"{"meta":{"traceEvents":[9]},"traceEvents":[{"x":[1]}, 2],"other":[3]}"#;
        for chunk in [1, 3, 4096] {
            assert_eq!(elements(object, chunk), [r#"{"x":[1]}"#, "2"], "chunks of {chunk}");
        }
    }

    #[test]
    fn pairs_begin_and_end_per_thread_and_keeps_long_spans() {
        let p = parse(
            r#"[
            {"name":"process_name","ph":"M","ts":0,"pid":1,"tid":1},
            {"name":"checkSourceFile","cat":"check","ph":"B","ts":100,"pid":1,"tid":1,"args":{"path":"/r/a.ts"}},
            {"name":"checkExpression","cat":"check","ph":"X","ts":110,"dur":50,"pid":1,"tid":1,"args":{"pos":3,"end":9}},
            {"name":"checkSourceFile","cat":"check","ph":"B","ts":120,"pid":1,"tid":2,"args":{"path":"/r/b.ts"}},
            {"name":"checkSourceFile","cat":"check","ph":"E","ts":300,"pid":1,"tid":1,"args":{"extra":1}},
            {"name":"findSourceFile","cat":"program","ph":"X","ts":1,"dur":1,"args":{"fileName":"/r/node_modules/@a/b/node_modules/cc/x.d.ts"}},
            {"name":"other","ph":"E","ts":400}
        ]"#,
            10.0,
        );
        let names: Vec<(&str, f64, f64)> =
            p.spans.iter().map(|s| (&*s.event.name, s.start, s.end)).collect();
        assert_eq!(names, [("checkExpression", 110.0, 160.0), ("checkSourceFile", 100.0, 300.0)]);
        assert_eq!(p.spans[1].event.arg_str("path"), Some("/r/a.ts"));
        assert_eq!(p.spans[1].event.arg("extra"), Some(&Value::from(1)));
        assert_eq!((p.min_time, p.max_time), (1.0, 300.0));
        assert_eq!(p.unclosed.len(), 1);
        assert_ne!(p.unclosed[0].stack, p.spans[1].event.stack);
        assert_eq!(p.unmatched_ends, 1);
        assert_eq!(p.checked_files, 1);
        assert_eq!(p.sample_files, ["/r/a.ts"]);
        assert_eq!(
            p.packages,
            [
                ("@a/b".to_string(), vec!["/r/node_modules/@a/b".to_string()]),
                ("cc".to_string(), vec!["/r/node_modules/@a/b/node_modules/cc".to_string()]),
            ]
        );
    }

    #[test]
    fn a_trace_cut_short_keeps_what_came_before() {
        let p = parse(
            "[\n{\"name\":\"checkSourceFile\",\"cat\":\"check\",\"ph\":\"B\",\"ts\":1,\"args\":{\"path\":\"/a.ts\"}},\n{\"name\":\"checkExpression\",\"ph\":\"X\",\"ts\":2,\"dur\":5},\n{\"name\":\"chec",
            0.0,
        );
        assert_eq!(p.spans.len(), 1);
        assert_eq!(p.unclosed.len(), 1);
        assert!(parse_reader("".as_bytes(), 0.0).is_err());
        assert!(parse_reader("garbage".as_bytes(), 0.0).is_err());
        assert!(parse_reader("[".as_bytes(), 0.0).is_err());
        assert!(parse_reader("[1, 2]".as_bytes(), 0.0).is_err());
        assert!(parse_reader("[]".as_bytes(), 0.0).unwrap().spans.is_empty());
    }

    #[test]
    fn tolerates_odd_events() {
        let p = parse(
            r#"[1, "x", {"ph":"X"}, {"ph":"Q","ts":1}, {"ph":"X","ts":"5","dur":"2","name":"n"},
                {"ph":"X","ts":1,"name":"nodur"}, {"ph":"B","ts":2,"name":"a","pid":"p","tid":7},
                {"ph":"B","ts":3,"name":"a","pid":"p","tid":"7"}]"#,
            0.0,
        );
        let spans: Vec<(f64, f64)> = p.spans.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(spans, [(5.0, 7.0), (1.0, 1.0)]);
        // `7` and `"7"` are one thread, as JavaScript prints them.
        assert_eq!(p.unclosed.len(), 2);
        assert_eq!(p.unclosed[0].stack, p.unclosed[1].stack);
        let p = parse(r#"{"traceEvents":[{"ph":"X","ts":1,"dur":3,"name":"n"}],"other":{}}"#, 0.0);
        assert_eq!(p.spans.len(), 1);
    }

    #[test]
    fn an_event_that_cant_be_read_loses_only_itself() {
        let deep = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
        let text = format!(
            r#"[{{"ph":"X","ts":1e400,"dur":1,"name":"huge"}},
               {{"ph":"X","ts":1,"dur":1,"name":"deep","args":{{"x":{deep}}}}},
               {{"ph":"X","ts":2,"dur":1,"name":"big","args":{{"x":1e400,"pos":1}}}},
               {{"ph":"X","ts":3,"dur":1,"name":"after"}}]"#
        );
        let p = parse(&text, 0.0);
        let names: Vec<&str> = p.spans.iter().map(|s| &*s.event.name).collect();
        assert_eq!(names, ["deep", "big", "after"]);
        assert!(p.spans.iter().all(|s| s.event.args.is_none()));
    }

    #[test]
    fn finds_package_dirs_as_analyze_trace() {
        assert_eq!(
            package_dirs("/r/node_modules/.pnpm/zod@3.0.0/node_modules/zod/index.d.ts"),
            [
                (".pnpm", "/r/node_modules/.pnpm"),
                ("zod", "/r/node_modules/.pnpm/zod@3.0.0/node_modules/zod")
            ]
        );
        assert_eq!(package_dirs("/r/node_modules/@scope"), Vec::<(&str, &str)>::new());
        assert_eq!(package_dirs("/r/node_modules/@s/p"), [("@s/p", "/r/node_modules/@s/p")]);
        assert_eq!(package_dirs("/r/node_modules/a"), Vec::<(&str, &str)>::new());
        assert_eq!(package_dirs("/r/node_modules/ab"), [("ab", "/r/node_modules/ab")]);
        assert_eq!(package_dirs("C:/r/node_modules/é/x"), Vec::<(&str, &str)>::new());
    }
}
