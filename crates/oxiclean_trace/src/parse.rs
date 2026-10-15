//! `trace.json`: the Chrome trace events `tsc --generateTrace` writes,
//! read as analyze-trace's `parse-trace-file.ts` reads them.
//!
//! The file is streamed, one event at a time: a big project's trace runs
//! to gigabytes, and only spans at least `skipMillis` long are kept. Begin
//! (`B`) and end (`E`) events are paired per process and thread; complete
//! (`X`) events carry their duration. A trace cut short, as when `tsc`
//! crashed, keeps every event before the cut, and the spans still open
//! there run to the end of the trace.

use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde_json::{Map, Value};
use tracing::{debug, warn};

use crate::paths::slashes;

/// One trace event: a span's start or end, or a whole span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub name: String,
    /// `check`, `checkTypes`, `emit`, `program`, ….
    pub cat: Option<String>,
    /// Microseconds since `tsc` started.
    pub ts: f64,
    /// The process and thread, which nest spans separately.
    pub stack_key: String,
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
    /// End events with no begin.
    pub unmatched_ends: Vec<Event>,
    /// Each package a loaded or parsed file is in, with each directory it
    /// was loaded from, in the order first seen.
    pub packages: Vec<(String, Vec<String>)>,
    /// How many files the trace checked.
    pub checked_files: usize,
    /// Why the trace stopped before its end, when it did.
    pub truncated: Option<String>,
}

/// Reads the trace at `path`, keeping spans at least `min_duration`
/// microseconds long.
///
/// # Errors
/// Fails when the file can't be opened or isn't a JSON array.
pub(crate) fn parse_file(path: &Path, min_duration: f64) -> std::io::Result<Parsed> {
    let file = File::open(path)?;
    parse_reader(BufReader::with_capacity(1 << 16, file), min_duration)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// As [`parse_file`], from `reader`.
pub(crate) fn parse_reader(reader: impl Read, min_duration: f64) -> Result<Parsed, String> {
    let mut state = State::new(min_duration);
    let mut de = serde_json::Deserializer::from_reader(reader);
    let result = Events(&mut state).deserialize(&mut de);
    match result {
        // Items, but none of them events.
        Ok(()) if state.items > 0 && state.events == 0 => {
            return Err("not a trace (it holds no events)".into());
        }
        Ok(()) => {}
        // Nothing read: not a trace at all.
        Err(e) if state.events == 0 => return Err(format!("not a trace ({e})")),
        Err(e) => {
            warn!("The trace is cut short or malformed after {} events: {e}", state.events);
            state.out.truncated = Some(e.to_string());
        }
    }
    Ok(state.finish())
}

struct State {
    min_duration: f64,
    out: Parsed,
    stacks: HashMap<String, Vec<Event>>,
    /// The order stacks were first used, so unclosed events come out in a
    /// stable order.
    stack_order: Vec<String>,
    package_index: HashMap<String, usize>,
    /// Objects read.
    events: usize,
    /// Items read, events or not.
    items: usize,
}

impl State {
    fn new(min_duration: f64) -> Self {
        Self {
            min_duration,
            out: Parsed { min_time: f64::INFINITY, max_time: 0.0, ..Parsed::default() },
            stacks: HashMap::new(),
            stack_order: Vec::new(),
            package_index: HashMap::new(),
            events: 0,
            items: 0,
        }
    }

    fn stack(&mut self, key: &str) -> &mut Vec<Event> {
        if !self.stacks.contains_key(key) {
            self.stack_order.push(key.to_string());
        }
        self.stacks.entry(key.to_string()).or_default()
    }

    fn add(&mut self, value: Value) {
        self.items += 1;
        let Value::Object(mut obj) = value else { return };
        self.events += 1;
        let ph = obj.get("ph").and_then(Value::as_str).unwrap_or_default().to_string();
        // Metadata and instant events say nothing about time spent.
        if matches!(ph.as_str(), "M" | "i" | "I") {
            return;
        }
        let Some(ts) = number(obj.get("ts")) else {
            debug!("Skipping a trace event without a time");
            return;
        };
        let event = Event {
            name: obj.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
            cat: obj.get("cat").and_then(Value::as_str).map(str::to_string),
            ts,
            stack_key: format!("{}:{}", id(obj.get("pid")), id(obj.get("tid"))),
            args: match obj.remove("args") {
                Some(Value::Object(args)) => Some(args),
                _ => None,
            },
        };
        let span = match ph.as_str() {
            "B" => {
                let key = event.stack_key.clone();
                self.stack(&key).push(event);
                return;
            }
            "E" => {
                let key = event.stack_key.clone();
                let stack = self.stack(&key);
                let matches =
                    stack.last().is_some_and(|b| b.name == event.name && b.cat == event.cat);
                if !matches {
                    self.out.unmatched_ends.push(event);
                    return;
                }
                let Some(begin) = stack.pop() else { return };
                let start = begin.ts;
                let end = event.ts;
                Span { event: merge(begin, event), start, end }
            }
            "X" => {
                let dur = number(obj.get("dur")).unwrap_or(0.0).max(0.0);
                Span { start: event.ts, end: event.ts + dur, event }
            }
            other => {
                debug!("Skipping a trace event of unknown phase {other:?}");
                return;
            }
        };
        self.out.min_time = self.out.min_time.min(span.start);
        self.out.max_time = self.out.max_time.max(span.end);
        if span.event.name == "checkSourceFile" {
            self.out.checked_files += 1;
        }
        // Before spans are dropped for being short. analyze-trace reads only
        // `findSourceFile`, which `tsc` writes only when it took long enough
        // to be sampled; `createSourceFile` is written for every file parsed.
        let loaded = match span.event.name.as_str() {
            "findSourceFile" => span.event.arg_str("fileName"),
            "createSourceFile" => span.event.arg_str("path"),
            _ => None,
        };
        if let Some(file) = loaded {
            self.record_packages(&slashes(file));
        }
        if span.end - span.start >= self.min_duration {
            self.out.spans.push(span);
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
        for key in &self.stack_order {
            if let Some(stack) = self.stacks.remove(key) {
                self.out.unclosed.extend(stack);
            }
        }
        if self.out.min_time > self.out.max_time {
            self.out.min_time = self.out.max_time;
        }
        self.out
    }
}

/// Each package directory in `file` with the package's name: for
/// `/r/node_modules/@a/b/node_modules/cc/x.d.ts`, `@a/b` at
/// `/r/node_modules/@a/b` and `c` at `/r/node_modules/@a/b/node_modules/c`.
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

/// `+value` in JavaScript, for a number or a numeric string.
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// A process or thread id as JavaScript prints it, `1` when absent.
fn id(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "1".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// A begin event with the arguments of its end added.
fn merge(mut begin: Event, end: Event) -> Event {
    if let Some(end_args) = end.args {
        begin.args.get_or_insert_with(Map::new).extend(end_args);
    }
    begin
}

/// Feeds a JSON array's elements to a [`State`] as they're read.
struct Events<'s>(&'s mut State);

impl<'de> DeserializeSeed<'de> for Events<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Events<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of trace events")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Value>()? {
            self.0.add(value);
        }
        Ok(())
    }

    /// The JSON object form, `{ "traceEvents": [...] }`, other tools write.
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "traceEvents" {
                map.next_value_seed(Events(&mut *self.0))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, min: f64) -> Parsed {
        parse_reader(text.as_bytes(), min).unwrap()
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
            p.spans.iter().map(|s| (s.event.name.as_str(), s.start, s.end)).collect();
        assert_eq!(names, [("checkExpression", 110.0, 160.0), ("checkSourceFile", 100.0, 300.0)]);
        assert_eq!(p.spans[1].event.arg_str("path"), Some("/r/a.ts"));
        assert_eq!(p.spans[1].event.arg("extra"), Some(&Value::from(1)));
        assert_eq!((p.min_time, p.max_time), (1.0, 300.0));
        assert_eq!(p.unclosed.len(), 1);
        assert_eq!(p.unclosed[0].stack_key, "1:2");
        assert_eq!(p.unmatched_ends.len(), 1);
        assert_eq!(p.checked_files, 1);
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
        assert!(p.truncated.is_some());
        assert_eq!(p.spans.len(), 1);
        assert_eq!(p.unclosed.len(), 1);
        assert!(parse_reader("".as_bytes(), 0.0).is_err());
        assert!(parse_reader("garbage".as_bytes(), 0.0).is_err());
        assert!(parse_reader("[".as_bytes(), 0.0).is_err());
    }

    #[test]
    fn tolerates_odd_events() {
        let p = parse(
            r#"[1, "x", {"ph":"X"}, {"ph":"Q","ts":1}, {"ph":"X","ts":"5","dur":"2","name":"n"},
                {"ph":"X","ts":1,"name":"nodur"}, {"ph":"B","ts":2,"name":"a","pid":"p","tid":7}]"#,
            0.0,
        );
        let spans: Vec<(f64, f64)> = p.spans.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(spans, [(5.0, 7.0), (1.0, 1.0)]);
        assert_eq!(p.unclosed[0].stack_key, "p:7");
        let p = parse(r#"{"traceEvents":[{"ph":"X","ts":1,"dur":3,"name":"n"}],"other":{}}"#, 0.0);
        assert_eq!(p.spans.len(), 1);
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
