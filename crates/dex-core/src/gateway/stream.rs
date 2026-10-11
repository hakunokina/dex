//! Incremental reducer adapted from Raine reducer/live_stream/accumulate/stream.
//! One state per HTTP request; parallel upstream blocks are serialized downstream.
use super::*;
use std::io::{BufRead, Read};

pub(super) struct Frames<R> {
    reader: R,
    bytes: usize,
}
impl<R: BufRead> Frames<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, bytes: 0 }
    }
    pub fn next(&mut self) -> Result<Option<(Option<String>, String)>, Fault> {
        let mut data = vec![];
        let mut event = None;
        loop {
            let mut line = Vec::new();
            // A bounded reader prevents a peer allocating an unbounded single line.
            let count = self
                .reader
                .by_ref()
                .take((MAX_BYTES + 1) as u64)
                .read_until(b'\n', &mut line)
                .map_err(|_| Fault::new(ErrorKind::Transport))?;
            self.bytes += count;
            if self.bytes > MAX_BYTES {
                return Err(Fault::new(ErrorKind::Limit));
            }
            if count == 0 {
                return if data.is_empty() && event.is_none() {
                    Ok(None)
                } else {
                    Err(Fault::new(ErrorKind::Eof))
                };
            }
            let line = std::str::from_utf8(&line)
                .map_err(|_| Fault::new(ErrorKind::Protocol))?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    return Ok(Some((event, data.join("\n"))));
                }
                event = None;
                continue;
            }
            if let Some(s) = line.strip_prefix("data:") {
                data.push(s.strip_prefix(' ').unwrap_or(s).to_owned());
            } else if let Some(s) = line.strip_prefix("event:") {
                event = Some(s.trim_start().to_owned());
            }
        }
    }
}

fn protocol() -> Fault {
    Fault::new(ErrorKind::Protocol)
}
fn unknown_event(kind: &str) {
    // Only bounded event-type identifiers; never log an event body or arbitrary strings.
    if kind.len() <= 128
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        log::debug!("gateway ignored SSE event: {kind}");
    }
}
fn index(v: &Value, field: &str) -> Result<u64, Fault> {
    v[field].as_u64().ok_or_else(protocol)
}
struct Block {
    value: Value,
    data: String,
    emitted: usize,
    started: bool,
    done: bool,
    stopped: bool,
}
impl Block {
    fn new(value: Value) -> Self {
        Self {
            value,
            data: String::new(),
            emitted: 0,
            started: false,
            done: false,
            stopped: false,
        }
    }
    fn kind(&self) -> &str {
        self.value["type"].as_str().unwrap_or("")
    }
    fn close(&mut self) -> Result<(), Fault> {
        match self.kind() {
            "text" => self.value["text"] = json!(self.data),
            "thinking" => self.value["thinking"] = json!(self.data),
            "tool_use" => {
                let args: Value = serde_json::from_str(&self.data).map_err(|_| protocol())?;
                if !args.is_object() {
                    return Err(protocol());
                }
                request::string(&self.value, "id")?;
                request::string(&self.value, "name")?;
                self.value["input"] = args;
            }
            _ => return Err(protocol()),
        }
        self.done = true;
        Ok(())
    }
}

pub(super) struct Stream<'a> {
    route: &'a Route,
    message: Value,
    blocks: Vec<Block>,
    keys: HashMap<String, usize>,
    reasoning: HashMap<String, Value>,
    next: usize,
    started: bool,
    pub complete: bool,
    chat_finish: Option<String>,
}

pub(super) struct Native {
    message: Option<Value>,
    active: Option<usize>,
    arguments: String,
    terminal: Option<Value>,
    pub complete: bool,
}
impl Native {
    pub fn new() -> Self {
        Self {
            message: None,
            active: None,
            arguments: String::new(),
            terminal: None,
            complete: false,
        }
    }
    pub fn feed(&mut self, data: &str) -> Result<Vec<Value>, Fault> {
        let v: Value = serde_json::from_str(data).map_err(|_| protocol())?;
        let t = request::string(&v, "type")?;
        if self.complete {
            return Err(protocol());
        }
        if t == "error" {
            return Err(super::classify(None, &v, None));
        }
        if t == "message_start" {
            if self.message.is_some()
                || !v["message"]["content"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
            {
                return Err(protocol());
            }
            self.message = Some(v["message"].clone());
            return Ok(vec![v]);
        }
        let message = self.message.as_mut().ok_or_else(protocol)?;
        if self.terminal.is_some() && t != "message_stop" && t != "ping" {
            return Err(protocol());
        }
        match t {
            "ping" => {}
            "content_block_start" => {
                let blocks = message["content"].as_array_mut().ok_or_else(protocol)?;
                let i = index(&v, "index")? as usize;
                if self.active.is_some() || i != blocks.len() || blocks.len() >= 1024 {
                    return Err(protocol());
                }
                blocks.push(v["content_block"].clone());
                self.active = Some(i);
                self.arguments.clear();
            }
            "content_block_delta" => {
                let i = index(&v, "index")? as usize;
                if self.active != Some(i) {
                    return Err(protocol());
                }
                let block = &mut message["content"][i];
                let delta = &v["delta"];
                let (field, part) = match request::string(delta, "type")? {
                    "text_delta" => ("text", "text"),
                    "thinking_delta" => ("thinking", "thinking"),
                    "signature_delta" => ("signature", "signature"),
                    "input_json_delta" => {
                        self.arguments
                            .push_str(delta["partial_json"].as_str().ok_or_else(protocol)?);
                        return Ok(vec![v]);
                    }
                    _ => return Err(Fault::new(ErrorKind::Unsupported)),
                };
                let text = format!(
                    "{}{}",
                    block[field].as_str().unwrap_or(""),
                    delta[part].as_str().ok_or_else(protocol)?
                );
                block[field] = json!(text);
            }
            "content_block_stop" => {
                let i = index(&v, "index")? as usize;
                if self.active != Some(i) {
                    return Err(protocol());
                }
                if !self.arguments.is_empty() {
                    let args: Value =
                        serde_json::from_str(&self.arguments).map_err(|_| protocol())?;
                    if !args.is_object() {
                        return Err(protocol());
                    }
                    message["content"][i]["input"] = args;
                }
                self.active = None;
            }
            "message_delta" => {
                if self.active.is_some() || !v["delta"]["stop_reason"].is_string() {
                    return Err(protocol());
                }
                for (key, value) in v["delta"].as_object().ok_or_else(protocol)? {
                    message[key] = value.clone();
                }
                if let Some(usage) = v["usage"].as_object() {
                    for (key, value) in usage {
                        message["usage"][key] = value.clone();
                    }
                }
                self.terminal = Some(v);
                return Ok(vec![]);
            }
            "message_stop" => {
                let delta = self.terminal.take().ok_or_else(protocol)?;
                if self.active.is_some() {
                    return Err(protocol());
                }
                self.complete = true;
                return Ok(vec![delta, v]);
            }
            _ => {
                unknown_event(t);
                return Ok(vec![]);
            }
        }
        Ok(vec![v])
    }
    pub fn message(self) -> Result<Value, Fault> {
        if !self.complete {
            return Err(Fault::new(ErrorKind::Eof));
        }
        self.message.ok_or_else(protocol)
    }
}
impl<'a> Stream<'a> {
    pub fn new(route: &'a Route) -> Self {
        Self {
            route,
            message: json!({"id":"msg_gateway","type":"message","role":"assistant","model":route.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}),
            blocks: vec![],
            keys: HashMap::new(),
            reasoning: HashMap::new(),
            next: 0,
            started: false,
            complete: false,
            chat_finish: None,
        }
    }
    fn add(&mut self, key: String, value: Value) -> Result<(), Fault> {
        if self.keys.contains_key(&key) || self.blocks.len() >= 1024 {
            return Err(protocol());
        }
        self.keys.insert(key, self.blocks.len());
        self.blocks.push(Block::new(value));
        Ok(())
    }
    fn block(&mut self, key: &str) -> Result<&mut Block, Fault> {
        let i = *self.keys.get(key).ok_or_else(protocol)?;
        self.blocks
            .get_mut(i)
            .filter(|b| !b.done)
            .ok_or_else(protocol)
    }
    fn sync(&mut self, events: &mut Vec<Value>) {
        if !self.started {
            self.started = true;
            events.push(json!({"type":"message_start","message":self.message}));
        }
        while let Some(block) = self.blocks.get_mut(self.next) {
            if !block.started {
                block.started = true;
                let mut value = block.value.clone();
                match block.kind() {
                    "text" => value["text"] = json!(""),
                    "thinking" => {
                        value["thinking"] = json!("");
                        value["signature"] = json!("");
                    }
                    _ => value["input"] = json!({}),
                }
                events.push(
                    json!({"type":"content_block_start","index":self.next,"content_block":value}),
                );
            }
            if block.emitted < block.data.len() {
                let text = &block.data[block.emitted..];
                let delta = match block.kind() {
                    "text" => json!({"type":"text_delta","text":text}),
                    "thinking" => json!({"type":"thinking_delta","thinking":text}),
                    _ => json!({"type":"input_json_delta","partial_json":text}),
                };
                events.push(json!({"type":"content_block_delta","index":self.next,"delta":delta}));
                block.emitted = block.data.len();
            }
            if !block.done {
                break;
            }
            if block.kind() == "thinking" {
                if let Some(signature) = block.value.get("signature") {
                    events.push(json!({"type":"content_block_delta","index":self.next,"delta":{"type":"signature_delta","signature":signature}}));
                }
            }
            block.stopped = true;
            events.push(json!({"type":"content_block_stop","index":self.next}));
            self.next += 1;
        }
    }
    fn usage(&mut self, usage: &Value, chat: bool) {
        let input = usage[if chat {
            "prompt_tokens"
        } else {
            "input_tokens"
        }]
        .as_u64()
        .unwrap_or(0);
        let cached = usage[if chat {
            "prompt_tokens_details"
        } else {
            "input_tokens_details"
        }]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
        self.message["usage"] = json!({"input_tokens":input.saturating_sub(cached),"output_tokens":usage[if chat {"completion_tokens"} else {"output_tokens"}].as_u64().unwrap_or(0),"cache_read_input_tokens":cached,"cache_creation_input_tokens":0});
    }
    fn finish(&mut self, stop: &str, events: &mut Vec<Value>) -> Result<(), Fault> {
        if self.blocks.is_empty() || self.blocks.iter().any(|b| !b.done) {
            return Err(protocol());
        }
        self.sync(events);
        self.message["content"] = json!(self
            .blocks
            .iter()
            .map(|b| b.value.clone())
            .collect::<Vec<_>>());
        self.message["stop_reason"] = json!(stop);
        events.push(json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":self.message["usage"]}));
        events.push(json!({"type":"message_stop"}));
        self.complete = true;
        Ok(())
    }
    fn response_item(&mut self, value: &Value, done: bool) -> Result<(), Fault> {
        let key = format!("item-{}", index(value, "output_index")?);
        let item = &value["item"];
        if !self.keys.contains_key(&key) {
            let block = match request::string(item, "type")? {
                "message" => json!({"type":"text","text":""}),
                "reasoning" => json!({"type":"thinking","thinking":""}),
                "function_call" => {
                    json!({"type":"tool_use","id":request::string(item,"call_id")?,"name":request::string(item,"name")?,"input":{}})
                }
                _ => return Err(Fault::new(ErrorKind::Unsupported)),
            };
            self.add(key.clone(), block)?;
        } else if !done {
            return Err(protocol());
        }
        if item["type"] == "reasoning" {
            let pending = self
                .reasoning
                .entry(key.clone())
                .or_insert_with(|| json!({"type":"reasoning"}));
            for field in ["id", "encrypted_content", "summary"] {
                if let Some(v) = item.get(field) {
                    pending[field] = v.clone();
                }
            }
            if done {
                let signature = request::signature(self.route, pending).map_err(|fault| {
                    if fault.kind == ErrorKind::InvalidRequest {
                        protocol()
                    } else {
                        fault
                    }
                })?;
                self.block(&key)?.value["signature"] = json!(signature);
            }
        }
        if done {
            let block = self.block(&key)?;
            let full = match block.kind() {
                "tool_use" => {
                    for (up, down) in [("call_id", "id"), ("name", "name")] {
                        if item.get(up).is_some_and(|v| v != &block.value[down]) {
                            return Err(protocol());
                        }
                    }
                    item["arguments"].as_str().map(str::to_owned)
                }
                "text" => item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|p| {
                                p[if p["type"] == "refusal" {
                                    "refusal"
                                } else {
                                    "text"
                                }]
                                .as_str()
                                .ok_or_else(protocol)
                            })
                            .collect::<Result<Vec<_>, _>>()
                            .map(|s| s.concat())
                    })
                    .transpose()?,
                "thinking" => item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .map(|p| p["text"].as_str().ok_or_else(protocol))
                            .collect::<Result<Vec<_>, _>>()
                            .map(|s| s.concat())
                    })
                    .transpose()?,
                _ => None,
            };
            if let Some(full) = full {
                if !block.data.is_empty() && block.data != full {
                    return Err(protocol());
                }
                block.data = full;
            }
            block.close()?;
        }
        Ok(())
    }
    fn responses(&mut self, v: &Value, events: &mut Vec<Value>) -> Result<(), Fault> {
        let t = request::string(v, "type")?;
        match t {
            "response.created" => {
                if let Some(id) = v["response"]["id"].as_str() {
                    self.message["id"] = json!(id);
                }
            }
            "response.output_item.added" => self.response_item(v, false)?,
            "response.output_item.done" => self.response_item(v, true)?,
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.function_call_arguments.delta"
            | "response.reasoning_summary_text.delta" => {
                let key = format!("item-{}", index(v, "output_index")?);
                let block = self.block(&key)?;
                let expected = match t {
                    "response.output_text.delta" | "response.refusal.delta" => "text",
                    "response.function_call_arguments.delta" => "tool_use",
                    _ => "thinking",
                };
                if block.kind() != expected {
                    return Err(protocol());
                }
                block
                    .data
                    .push_str(v["delta"].as_str().ok_or_else(protocol)?);
                if block.data.len() > MAX_BYTES {
                    return Err(Fault::new(ErrorKind::Limit));
                }
            }
            "response.completed" => {
                if v["response"]["status"] != "completed"
                    || v["response"].get("error").is_some_and(|e| !e.is_null())
                    || v["response"]
                        .get("incomplete_details")
                        .is_some_and(|e| !e.is_null())
                {
                    return Err(Fault::new(ErrorKind::Incomplete));
                }
                // The terminal snapshot can supply items omitted by a delta producer.
                if let Some(output) = v["response"]["output"].as_array() {
                    for (i, item) in output.iter().enumerate() {
                        let key = format!("item-{i}");
                        if self.keys.get(&key).is_none_or(|n| !self.blocks[*n].done) {
                            self.response_item(&json!({"output_index":i,"item":item}), true)?;
                        }
                    }
                }
                self.usage(&v["response"]["usage"], false);
                let stop = if self.blocks.iter().any(|b| b.kind() == "tool_use") {
                    "tool_use"
                } else {
                    "end_turn"
                };
                return self.finish(stop, events);
            }
            "response.failed" | "error" => return Err(super::classify(None, v, None)),
            "response.incomplete" => return Err(Fault::new(ErrorKind::Incomplete)),
            "response.in_progress"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done" => {}
            _ => {
                unknown_event(t);
                return Ok(());
            }
        }
        self.sync(events);
        Ok(())
    }
    fn chat(&mut self, v: &Value, events: &mut Vec<Value>) -> Result<(), Fault> {
        if v.get("error").is_some() {
            return Err(super::classify(None, v, None));
        }
        if let Some(id) = v["id"].as_str() {
            self.message["id"] = json!(id);
        }
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage(usage, true);
        }
        let choices = v["choices"].as_array().ok_or_else(protocol)?;
        for choice in choices {
            if choice["index"] != 0 {
                return Err(Fault::new(ErrorKind::Unsupported));
            }
            if self.chat_finish.is_some() {
                return Err(protocol());
            }
            if let Some(text) = choice["delta"]["content"].as_str() {
                if !self.keys.contains_key("text") {
                    self.add("text".into(), json!({"type":"text","text":""}))?;
                }
                self.block("text")?.data.push_str(text);
            }
            if let Some(tools) = choice["delta"]["tool_calls"].as_array() {
                for tool in tools {
                    let key = format!("tool-{}", index(tool, "index")?);
                    if !self.keys.contains_key(&key) {
                        self.add(key.clone(),json!({"type":"tool_use","id":request::string(tool,"id")?,"name":request::string(&tool["function"],"name")?,"input":{}}))?;
                    }
                    let block = self.block(&key)?;
                    if let Some(id) = tool["id"].as_str().filter(|s| !s.is_empty()) {
                        if Some(id) != block.value["id"].as_str() {
                            return Err(protocol());
                        }
                    }
                    if let Some(name) = tool["function"]["name"].as_str().filter(|s| !s.is_empty())
                    {
                        if Some(name) != block.value["name"].as_str() {
                            return Err(protocol());
                        }
                    }
                    if let Some(args) = tool["function"]["arguments"].as_str() {
                        block.data.push_str(args);
                    }
                }
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                let stop = match finish {
                    "stop" => "end_turn",
                    "tool_calls" => "tool_use",
                    "length" => return Err(Fault::new(ErrorKind::Incomplete)),
                    _ => return Err(Fault::new(ErrorKind::Unsupported)),
                };
                self.chat_finish = Some(stop.into());
                for block in &mut self.blocks {
                    block.close()?;
                }
            }
        }
        self.sync(events);
        Ok(())
    }
    pub fn feed(&mut self, data: &str) -> Result<Vec<Value>, Fault> {
        if self.complete {
            return Err(protocol());
        }
        let mut events = vec![];
        if data == "[DONE]" {
            if self.route.protocol != Protocol::OpenaiChat {
                return Err(protocol());
            }
            let stop = self
                .chat_finish
                .clone()
                .ok_or_else(|| Fault::new(ErrorKind::Eof))?;
            self.finish(&stop, &mut events)?;
        } else {
            let value: Value = serde_json::from_str(data).map_err(|_| protocol())?;
            if self.route.protocol == Protocol::OpenaiResponses {
                self.responses(&value, &mut events)?;
            } else {
                self.chat(&value, &mut events)?;
            }
        }
        Ok(events)
    }
    pub fn message(self) -> Result<Value, Fault> {
        if !self.complete {
            return Err(Fault::new(ErrorKind::Eof));
        }
        Ok(self.message)
    }
}
