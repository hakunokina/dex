//! Narrow adaptation of Raine request.rs: function/history/image/effort mapping.
//! No global env overrides, hosted tools, compaction or private-backend fields.
use super::*;

fn invalid() -> Fault {
    Fault::new(ErrorKind::InvalidRequest)
}
fn unsupported() -> Fault {
    Fault::new(ErrorKind::Unsupported)
}
pub(super) fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, Fault> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)
}
fn blocks(v: &Value) -> Result<Vec<Value>, Fault> {
    if let Some(text) = v.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    v.as_array().cloned().ok_or_else(invalid)
}
fn text(v: &Value) -> Result<String, Fault> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_owned());
    }
    blocks(v)?
        .iter()
        .map(|b| {
            if b["type"] != "text" {
                return Err(unsupported());
            }
            b["text"].as_str().map(str::to_owned).ok_or_else(invalid)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|s| s.join("\n"))
}
fn image(block: &Value) -> Result<String, Fault> {
    let src = &block["source"];
    match src["type"].as_str() {
        Some("base64") => {
            let mime = string(src, "media_type")?;
            if !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) {
                return Err(unsupported());
            }
            Ok(format!("data:{mime};base64,{}", string(src, "data")?))
        }
        Some("url") => Ok(string(src, "url")?.to_owned()),
        _ => Err(unsupported()),
    }
}
fn part(block: &Value, chat: bool, assistant: bool) -> Result<Value, Fault> {
    match block["type"].as_str() {
        Some("text") => Ok(
            json!({"type":if chat {"text"} else if assistant {"output_text"} else {"input_text"},"text":block["text"].as_str().ok_or_else(invalid)?}),
        ),
        Some("image") if !assistant => {
            let url = image(block)?;
            Ok(if chat {
                json!({"type":"image_url","image_url":{"url":url}})
            } else {
                json!({"type":"input_image","image_url":url})
            })
        }
        _ => Err(unsupported()),
    }
}
fn effort(input: &Value) -> Result<Option<&str>, Fault> {
    if input.pointer("/thinking/type") == Some(&json!("enabled")) {
        return Err(unsupported());
    }
    match input.pointer("/output_config/effort") {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s @ ("medium" | "high" | "xhigh")) => Ok(Some(s)),
            _ => Err(unsupported()),
        },
    }
}
fn tools(input: &Value) -> Result<Vec<Value>, Fault> {
    let Some(value) = input.get("tools") else {
        return Ok(vec![]);
    };
    value.as_array().ok_or_else(invalid)?.iter().map(|tool| {
        if tool.get("type").is_some() || tool.get("defer_loading").is_some() { return Err(unsupported()); }
        let schema = tool.get("input_schema").filter(|s| s.is_object()).ok_or_else(invalid)?;
        let mut function = json!({"type":"function","name":string(tool,"name")?,"parameters":schema,"strict":false});
        if let Some(description) = tool.get("description") { function["description"] = description.clone(); }
        Ok(function)
    }).collect()
}
fn choice(input: &Value, chat: bool, namespace: bool) -> Result<Option<Value>, Fault> {
    let Some(v) = input.get("tool_choice") else {
        return Ok(None);
    };
    Ok(Some(match string(v, "type")? {
        "auto" => json!("auto"),
        "any" => json!("required"),
        "none" => json!("none"),
        "tool" => {
            let name = string(v, "name")?;
            if chat {
                json!({"type":"function","function":{"name":name}})
            } else if namespace {
                json!({"type":"function","namespace":"dex","name":name})
            } else {
                json!({"type":"function","name":name})
            }
        }
        _ => return Err(unsupported()),
    }))
}
const REPLAY_PREFIX: &str = "dex:responses:v1:";
fn scope(route: &Route) -> Value {
    json!([route.provider, route.account, route.model])
}
pub(super) fn signature(route: &Route, item: &Value) -> Result<String, Fault> {
    string(item, "id")?;
    string(item, "encrypted_content")?;
    let signature = format!(
        "{REPLAY_PREFIX}{}",
        json!({"scope":scope(route),"item":item})
    );
    if signature.len() > super::MAX_BYTES {
        return Err(Fault::new(ErrorKind::Limit));
    }
    Ok(signature)
}
fn replay(route: &Route, block: &Value) -> Result<Option<Value>, Fault> {
    let Some(raw) = block.get("signature").and_then(Value::as_str) else {
        return Ok(None);
    };
    if raw.len() > super::MAX_BYTES {
        return Err(Fault::new(ErrorKind::Limit));
    }
    let Some(payload) = raw.strip_prefix(REPLAY_PREFIX) else {
        return Ok(None);
    };
    let Ok(envelope) = serde_json::from_str::<Value>(payload) else {
        return Ok(None);
    };
    if envelope["scope"] != scope(route) {
        return Ok(None);
    }
    let item = &envelope["item"];
    if item["type"] != "reasoning" {
        return Ok(None);
    }
    if string(item, "id").is_err() || string(item, "encrypted_content").is_err() {
        return Ok(None);
    }
    Ok(Some(item.clone()))
}
fn result(block: &Value, chat: bool) -> Result<Value, Fault> {
    let content = block.get("content").ok_or_else(invalid)?;
    let prefix = if block["is_error"] == true {
        "Error: "
    } else {
        ""
    };
    if let Some(s) = content.as_str() {
        return Ok(json!(format!("{prefix}{s}")));
    }
    let mut parts = blocks(content)?
        .iter()
        .map(|p| part(p, chat, false))
        .collect::<Result<Vec<_>, _>>()?;
    if !prefix.is_empty() {
        parts.insert(
            0,
            json!({"type":if chat {"text"} else {"input_text"},"text":prefix}),
        );
    }
    Ok(json!(parts))
}
fn responses_history(route: &Route, input: &Value) -> Result<Vec<Value>, Fault> {
    let mut out = vec![];
    for message in input["messages"].as_array().ok_or_else(invalid)? {
        let role = match string(message, "role")? {
            "system" => "developer",
            s @ ("user" | "assistant" | "developer") => s,
            _ => return Err(unsupported()),
        };
        let mut content = vec![];
        for block in blocks(&message["content"])? {
            let special = match block["type"].as_str() {
                Some("tool_use") if role == "assistant" => {
                    let args = block
                        .get("input")
                        .filter(|v| v.is_object())
                        .ok_or_else(invalid)?;
                    let mut call = json!({"type":"function_call","call_id":string(&block,"id")?,"name":string(&block,"name")?,"arguments":args.to_string()});
                    if route.profile == Profile::SiwcNamespace {
                        call["namespace"] = json!("dex");
                    }
                    Some(call)
                }
                Some("tool_result") if role == "user" => Some(
                    json!({"type":"function_call_output","call_id":string(&block,"tool_use_id")?,"output":result(&block,false)?}),
                ),
                Some("thinking") if role == "assistant" => replay(route, &block)?,
                _ => {
                    content.push(part(&block, false, role == "assistant")?);
                    None
                }
            };
            if let Some(item) = special {
                if !content.is_empty() {
                    out.push(json!({"type":"message","role":role,"content":std::mem::take(&mut content)}));
                }
                out.push(item);
            }
        }
        if !content.is_empty() {
            out.push(json!({"type":"message","role":role,"content":content}));
        }
    }
    Ok(out)
}
fn chat_history(input: &Value) -> Result<Vec<Value>, Fault> {
    let mut out = vec![];
    if let Some(system) = input.get("system") {
        out.push(json!({"role":"system","content":text(system)?}));
    }
    for message in input["messages"].as_array().ok_or_else(invalid)? {
        let role = string(message, "role")?;
        if !matches!(role, "user" | "assistant" | "system") {
            return Err(unsupported());
        }
        let mut content = vec![];
        let mut calls = vec![];
        for block in blocks(&message["content"])? {
            match block["type"].as_str() {
                Some("tool_use") if role == "assistant" => calls.push(json!({"type":"function","id":string(&block,"id")?,"function":{"name":string(&block,"name")?,"arguments":block.get("input").filter(|v| v.is_object()).ok_or_else(invalid)?.to_string()}})),
                Some("tool_result") if role == "user" => {
                    if !content.is_empty() { out.push(json!({"role":role,"content":std::mem::take(&mut content)})); }
                    out.push(json!({"role":"tool","tool_call_id":string(&block,"tool_use_id")?,"content":result(&block,true)?}));
                }
                _ => content.push(part(&block,true,role=="assistant")?),
            }
        }
        if !content.is_empty() || !calls.is_empty() {
            let mut m = json!({"role":role,"content":if content.is_empty() {Value::Null} else {json!(content)}});
            if !calls.is_empty() {
                m["tool_calls"] = json!(calls);
            }
            out.push(m);
        }
    }
    Ok(out)
}
pub(super) fn translate(route: &Route, input: &Value) -> Result<Value, Fault> {
    if !input.is_object() {
        return Err(invalid());
    }
    if route.protocol == Protocol::AnthropicMessages {
        let mut body = input.clone();
        body["model"] = json!(route.model);
        return Ok(body);
    }
    if input.get("context_management").is_some() || input.pointer("/output_config/format").is_some()
    {
        return Err(unsupported());
    }
    let chat = route.protocol == Protocol::OpenaiChat;
    if chat && route.model.starts_with("gpt-6.1-sol") {
        return Err(unsupported());
    }
    let functions = tools(input)?;
    let parallel = match input.pointer("/tool_choice/disable_parallel_tool_use") {
        Some(v) => !v.as_bool().ok_or_else(invalid)?,
        None => true,
    };
    let mut body = json!({"model":route.model,"stream":true});
    if !chat || !functions.is_empty() {
        body["parallel_tool_calls"] = json!(parallel);
        if let Some(choice) = choice(input, chat, route.profile == Profile::SiwcNamespace)? {
            body["tool_choice"] = choice;
        }
    }
    if let Some(effort) = effort(input)? {
        if chat {
            body["reasoning_effort"] = json!(effort);
        } else {
            body["reasoning"] = json!({"effort":effort,"summary":"auto"});
        }
    }
    if chat {
        body["messages"] = json!(chat_history(input)?);
        body["stream_options"] = json!({"include_usage":true});
        if !functions.is_empty() {
            body["tools"] = json!(functions
                .into_iter()
                .map(|mut f| {
                    f.as_object_mut().unwrap().remove("type");
                    json!({"type":"function","function":f})
                })
                .collect::<Vec<_>>());
        }
    } else {
        body["store"] = json!(false);
        body["include"] = json!(["reasoning.encrypted_content"]);
        let mut items = vec![];
        if !functions.is_empty() {
            match route.profile {
                Profile::SiwcAdditionalTools => items
                    .push(json!({"type":"additional_tools","role":"developer","tools":functions})),
                Profile::SiwcNamespace => {
                    body["tools"] = json!([{"type":"namespace","name":"dex","description":"Locally executed tools","tools":functions}])
                }
                Profile::ApiKey => body["tools"] = json!(functions),
            }
        }
        if let Some(system) = input.get("system") {
            items.push(json!({"type":"message","role":"developer","content":[{"type":"input_text","text":text(system)?}]}));
        }
        items.extend(responses_history(route, input)?);
        body["input"] = json!(items);
    }
    if route.profile == Profile::ApiKey {
        if let Some(max) = input.get("max_tokens") {
            body[if chat {
                "max_completion_tokens"
            } else {
                "max_output_tokens"
            }] = max.clone();
        }
        for name in ["temperature", "top_p"] {
            if let Some(v) = input.get(name) {
                body[name] = v.clone();
            }
        }
    }
    Ok(body)
}
