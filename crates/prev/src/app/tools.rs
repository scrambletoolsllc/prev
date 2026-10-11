//! The tools prev offers agents. Each has a name, a kind, which the
//! approval switches go by, a description the model reads, an input
//! schema made from its input type, and a function that runs it against
//! a window. A tool answers through its [`Answer`], at once or once the
//! document thread or a render replies.

use std::sync::LazyLock;

use base64::Engine;
use iced::{Task, window};
use prev::control::{Call, Error, code};
use prev::filetype::FileKind;
use prev_store::settings;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use prev::image::window as image_window;

use super::{Content, Message, Prev};

/// What a tool does, which decides whether prev asks first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "the tools of most kinds come in later steps")]
pub(crate) enum Kind {
    Read,
    View,
    Markup,
    Edit,
    Sign,
    Redact,
    Export,
}

impl Kind {
    pub(super) const ALL: [Kind; 7] = [
        Kind::Read,
        Kind::View,
        Kind::Markup,
        Kind::Edit,
        Kind::Sign,
        Kind::Redact,
        Kind::Export,
    ];

    /// Its switch in Settings: whether prev asks before a tool of this
    /// kind runs.
    pub(super) fn asks(self, ask: &settings::AskBefore) -> bool {
        match self {
            Kind::Read => ask.reading,
            Kind::View => ask.viewing,
            Kind::Markup => ask.marking_up,
            Kind::Edit => ask.editing,
            Kind::Sign => ask.signing,
            Kind::Redact => ask.redacting,
            Kind::Export => ask.exporting,
        }
    }

    pub(super) fn set_asks(self, ask: &mut settings::AskBefore, asks: bool) {
        *match self {
            Kind::Read => &mut ask.reading,
            Kind::View => &mut ask.viewing,
            Kind::Markup => &mut ask.marking_up,
            Kind::Edit => &mut ask.editing,
            Kind::Sign => &mut ask.signing,
            Kind::Redact => &mut ask.redacting,
            Kind::Export => &mut ask.exporting,
        } = asks;
    }

    /// Whether Undo cannot take a tool of this kind back.
    pub(super) fn is_final(self) -> bool {
        matches!(self, Kind::Sign | Kind::Redact | Kind::Export)
    }
}

/// What a tool gives back.
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// An object, as structured content and the same as text for agents
    /// that read only text.
    Json(Value),
    Text(String),
    /// A picture, with a line saying what it shows.
    Png(Vec<u8>, String),
}

impl Output {
    /// The output as an MCP tool result.
    fn into_result(self) -> Value {
        match self {
            Output::Json(value) => json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "structuredContent": value,
            }),
            Output::Text(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Output::Png(png, note) => json!({
                "content": [
                    {
                        "type": "image",
                        "data": base64::engine::general_purpose::STANDARD.encode(png),
                        "mimeType": "image/png",
                    },
                    { "type": "text", "text": note },
                ],
            }),
        }
    }
}

/// Where a tool's answer goes. A tool that waits on something keeps it
/// and answers later, with [`Answer::later`]; dropped unanswered, it tells
/// the agent prev did not answer.
#[derive(Debug, Clone)]
pub(crate) struct Answer(Call);

impl Answer {
    pub(super) fn send(&self, result: Result<Output, Error>) {
        self.0.reply(result.map(Output::into_result));
    }

    /// Tells the agent what the call is waiting for, before its answer.
    pub(super) fn note(&self, message: &str) {
        self.0
            .note(super::agents::WAITING, json!({ "message": message }));
    }

    /// Answers with what `task` gives, once it finishes.
    pub(super) fn later(self, task: Task<Result<Output, Error>>) -> Task<Message> {
        task.map(move |result| Message::ToolAnswer(self.clone(), result))
    }
}

type Run = Box<dyn Fn(&mut Prev, Value, Answer) -> Task<Message> + Send + Sync>;

pub(super) struct Tool {
    pub(super) name: &'static str,
    pub(super) title: &'static str,
    pub(super) kind: Kind,
    description: &'static str,
    schema: fn() -> Value,
    run: Run,
}

/// Tools that draw on an image's markup, which they open first.
const ON_IMAGE_MARKUP: [&str; 7] = [
    "add_note",
    "add_text_box",
    "add_shape",
    "edit_annotation",
    "delete_annotation",
    "place_image",
    "place_signature",
];

/// How often, and how many times, a tool waiting for an image's markup
/// to open looks again: up to ten seconds.
const MARKUP_WAIT: std::time::Duration = std::time::Duration::from_millis(200);
const MARKUP_TRIES: u32 = 50;

impl Tool {
    /// Runs the tool on `arguments`; it answers through `answer`. A tool
    /// that draws on an image opens the image's markup first, and runs
    /// once it is open.
    pub(super) fn start(&self, app: &mut Prev, arguments: Value, answer: Answer) -> Task<Message> {
        self.start_after(app, arguments, answer, 0)
    }

    pub(super) fn start_after(
        &self,
        app: &mut Prev,
        arguments: Value,
        answer: Answer,
        tries: u32,
    ) -> Task<Message> {
        if ON_IMAGE_MARKUP.contains(&self.name)
            && let Ok(id) = app.tool_window(arguments["window"].as_u64())
            && let Some(images) = app.images_mut(id)
            && images
                .agent_markup()
                .is_none_or(|(_, viewer, _)| viewer.is_none())
        {
            if tries >= MARKUP_TRIES {
                answer.send(Err(Error::new(
                    code::INTERNAL_ERROR,
                    "The image's markup did not open; it can be marked up only when it is not \
                     being edited, and not as an SVG drawing or animation.",
                )));
                return Task::none();
            }
            let opening = if images.agent_markup().is_none() && !images.markup_shown() {
                app.update(Message::Image(id, image_window::Message::ToggleMarkup))
            } else {
                Task::none()
            };
            let name = self.name;
            let wait = Task::perform(
                prev::image::editor::spawn(|| std::thread::sleep(MARKUP_WAIT)),
                move |_| Message::ToolDeferred(answer.clone(), name, arguments.clone(), tries + 1),
            );
            return Task::batch([opening, wait]);
        }
        (self.run)(app, arguments, answer)
    }
}

/// A tool whose input is `I`, read from the call's arguments.
fn tool<I: DeserializeOwned + JsonSchema + 'static>(
    name: &'static str,
    title: &'static str,
    kind: Kind,
    description: &'static str,
    run: fn(&mut Prev, I, &Answer) -> Task<Message>,
) -> Tool {
    Tool {
        name,
        title,
        kind,
        description,
        schema: || {
            let mut schema = serde_json::to_value(schemars::schema_for!(I)).unwrap_or_default();
            // The tool's own name and description say what these would.
            if let Some(schema) = schema.as_object_mut() {
                schema.remove("$schema");
                schema.remove("title");
                schema.remove("description");
                // Some clients, such as ollama's, need properties even when
                // there are none.
                schema.entry("properties").or_insert_with(|| json!({}));
            }
            compact(&mut schema);
            schema
        },
        run: Box::new(
            move |app, arguments, answer| match serde_json::from_value::<I>(arguments) {
                Ok(input) => run(app, input, &answer),
                Err(error) => {
                    answer.send(Err(Error::new(
                        code::INVALID_PARAMS,
                        format!("{name}: {error}"),
                    )));
                    Task::none()
                }
            },
        ),
    }
}

/// Leaves out what a schema says only for a validator, since every agent
/// reads the schemas on every turn and prev checks the input itself: the
/// number formats and their bounds, `null` beside optional fields' types,
/// shared definitions (written where they are used instead) and the
/// line breaks of descriptions.
fn compact(schema: &mut Value) {
    let definitions = schema
        .as_object_mut()
        .and_then(|schema| schema.remove("$defs"))
        .unwrap_or_default();
    schema
        .as_object_mut()
        .map(|schema| schema.remove("additionalProperties"));
    tidy(schema, &definitions);
}

fn tidy(value: &mut Value, definitions: &Value) {
    match value {
        Value::Object(object) => {
            // An optional field is one of a type or null: the type alone.
            if let Some(Value::Array(any)) = object.get("anyOf")
                && any.len() == 2
                && let Some(other) = any.iter().find(|one| one["type"] != "null")
                && any.iter().any(|one| one["type"] == "null")
            {
                let other = other.clone();
                object.remove("anyOf");
                if let Some(Value::Object(other)) = Some(other) {
                    for (key, value) in other {
                        object.entry(key).or_insert(value);
                    }
                }
            }
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                && let Some(Value::Object(definition)) = definitions.get(name).cloned()
            {
                object.remove("$ref");
                for (key, value) in definition {
                    object.entry(key).or_insert(value);
                }
            }
            if let Some(Value::Array(types)) = object.get("type")
                && types.len() == 2
                && let Some(only) = types.iter().find(|one| *one != "null").cloned()
            {
                object.insert("type".into(), only);
            }
            if let Some(format) = object.remove("format")
                && format
                    .as_str()
                    .is_some_and(|format| format.starts_with("uint"))
            {
                if object.get("minimum") == Some(&json!(0)) {
                    object.remove("minimum");
                }
                if format == "uint8" && object.get("maximum") == Some(&json!(255)) {
                    object.remove("maximum");
                }
            }
            if let Some(Value::String(description)) = object.get_mut("description") {
                *description = description.replace('\n', " ");
            }
            for (key, value) in object.iter_mut() {
                // Field names are not schemas, but their values are.
                if key != "enum" && key != "required" {
                    tidy(value, definitions);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|value| tidy(value, definitions)),
        _ => {}
    }
}

mod edit;
mod finish;
mod markup;
pub(crate) use markup::Route;
mod read;
mod view;

/// Every tool, in the order agents see them.
static TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    let mut tools = vec![
        tool(
            "list_windows",
            "List windows",
            Kind::Read,
            "Lists prev's open windows: each one's number, title, kind (pdf, image, svg, \
             markdown, or start for a window with no file), file path, size in logical pixels, \
             and whether it is the one in front. Other tools take the number as `window`.",
            |app, _: Nothing, answer| {
                answer.send(Ok(Output::Json(json!({ "windows": app.window_list() }))));
                Task::none()
            },
        ),
        tool(
            "focus_window",
            "Focus a window",
            Kind::View,
            "Brings a prev window to the front and gives it the keyboard focus. Some \
             desktops, such as Wayland ones, only flag the window for the user instead.",
            |app, on: On, answer| match app.tool_window(on.window) {
                Ok(id) => {
                    answer.send(Ok(Output::Text(format!(
                        "Asked to bring window {} to the front. Some desktops flag the \
                         window for the user instead.",
                        number(id)
                    ))));
                    bring_forward(id)
                }
                Err(error) => {
                    answer.send(Err(error));
                    Task::none()
                }
            },
        ),
    ];
    tools.extend(read::tools());
    tools.extend(view::tools());
    tools.extend(markup::tools());
    tools.extend(edit::tools());
    tools.extend(finish::tools());
    tools
});

/// The input of a tool that takes none.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Nothing {}

/// The input of a tool that acts on a window and takes nothing else.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct On {
    /// The window's number from list_windows; the one in front if left out.
    window: Option<u64>,
}

/// Answers `answer` with `result` at once.
fn reply(answer: &Answer, result: Result<Output, Error>) -> Task<Message> {
    answer.send(result);
    Task::none()
}

/// The tools as MCP lists them; those whose kind `ask` makes prev ask
/// about say so, so agents know a call may wait for the user.
pub(super) fn list(ask: &settings::AskBefore) -> Value {
    TOOLS
        .iter()
        .map(|tool| {
            let description = if tool.kind.asks(ask) {
                format!(
                    "{} prev asks the user to allow it first, in the window it acts on, so the \
                     call waits for their answer: tell them to look at prev.",
                    tool.description
                )
            } else {
                tool.description.to_owned()
            };
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": description,
                "inputSchema": (tool.schema)(),
                "annotations": annotations(tool.kind),
            })
        })
        .collect()
}

/// The tools as the assistant panel's chat gets them.
pub(super) fn specs(ask: &settings::AskBefore) -> Vec<prev_assist::ToolSpec> {
    list(ask)
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| prev_assist::ToolSpec {
            name: tool["name"].as_str().unwrap_or_default().to_owned(),
            description: tool["description"].as_str().unwrap_or_default().to_owned(),
            schema: tool["inputSchema"].clone(),
        })
        .collect()
}

/// The tools the assistant gets in window `id`: those that work on what
/// it shows, so a model reads fewer tool definitions on every turn, and
/// the window tools only when there are windows to choose between.
pub(super) fn assistant_specs(app: &Prev, id: window::Id) -> Vec<prev_assist::ToolSpec> {
    let shows = read::shows(app, id);
    let several = app.windows.len() > 1;
    specs(&settings::AskBefore::never())
        .into_iter()
        .filter(|tool| fits(&tool.name, shows, several))
        .map(|mut tool| {
            // With one window, the chat fills in its number itself.
            if !several && let Some(properties) = tool.schema["properties"].as_object_mut() {
                properties.remove("window");
            }
            tool
        })
        .collect()
}

/// Whether the tool named `name` works on a window showing `shows`.
fn fits(name: &str, shows: read::Shows, several: bool) -> bool {
    use read::Shows;
    // The window tools: always to open, close or size the user's window,
    // and to choose between windows when there are more than one.
    if matches!(name, "list_windows" | "focus_window") {
        return several;
    }
    let image_only = [
        "render_image",
        "get_image",
        "crop",
        "rotate",
        "resize",
        "adjust_color",
        "replace_image",
    ];
    let pdf_only = [
        "page_text",
        "search",
        "form_fields",
        "fill_field",
        "render_page",
        "highlight",
        "set_view_mode",
        "scroll_to",
        "apply_redactions",
        "rotate_pages",
        "delete_pages",
        "move_pages",
        "insert_blank_page",
        "crop_pages",
    ];
    let markdown = [
        "document_info",
        "page_text",
        "search",
        "current_view",
        "set_zoom",
        "point_at",
        "show_panel",
        "hide_panel",
        "export",
        "open_file",
        "set_window",
        "close_window",
    ];
    match shows {
        // A window without a file yet may open any kind.
        Shows::Start => true,
        Shows::Pdf => !image_only.contains(&name),
        Shows::Image => !pdf_only.contains(&name),
        Shows::Markdown => markdown.contains(&name),
    }
}

/// What window `id` is, for the assistant's instructions: facts that stay
/// put while a chat goes on, so it need not ask for them first.
pub(super) fn summary(app: &Prev, id: window::Id) -> Option<String> {
    read::summary(app, id)
}

/// What the user sees in window `id` as they write, sent with each of
/// their messages, as it changes while they read.
pub(super) fn view_note(app: &Prev, id: window::Id) -> Option<String> {
    read::view_note(app, id)
}

/// Whether the tool named `name` takes a window number.
pub(super) fn takes_window(name: &str) -> bool {
    find(name).is_some_and(|tool| !(tool.schema)()["properties"]["window"].is_null())
}

/// MCP's hints for clients: what only reads, and what Undo cannot take
/// back.
fn annotations(kind: Kind) -> Value {
    match kind {
        Kind::Read => json!({ "readOnlyHint": true }),
        Kind::View | Kind::Markup | Kind::Edit => {
            json!({ "readOnlyHint": false, "destructiveHint": false })
        }
        Kind::Sign | Kind::Redact | Kind::Export => {
            json!({ "readOnlyHint": false, "destructiveHint": true })
        }
    }
}

/// The tool named `name`.
pub(super) fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Brings window `id` to the front. Wayland lets a window ask only for
/// attention, which some compositors answer by focusing it.
pub(super) fn bring_forward(id: window::Id) -> Task<Message> {
    let focus = window::gain_focus(id);
    if cfg!(target_os = "linux") {
        Task::batch([
            focus,
            window::request_user_attention(id, Some(window::UserAttention::Informational)),
        ])
    } else {
        focus
    }
}

/// The number agents know window `id` by.
pub(super) fn number(id: window::Id) -> u64 {
    id.to_string().parse().unwrap_or_default()
}

impl Prev {
    /// Runs the tool a `tools/call` names, which answers `call`, once
    /// the user allows it when its kind asks first. `agent` is the name
    /// the user knows the agent by.
    pub(super) fn run_tool(&mut self, call: Call, agent: String) -> Task<Message> {
        self.run_tool_asking(call, agent, true)
    }

    /// Runs the tool for the assistant, which acts on what the user asks
    /// it in the chat: the switches for outside agents do not apply.
    pub(super) fn run_tool_for_user(&mut self, call: Call, agent: String) -> Task<Message> {
        self.run_tool_asking(call, agent, false)
    }

    fn run_tool_asking(&mut self, call: Call, agent: String, outside: bool) -> Task<Message> {
        let name = call.params["name"].as_str().unwrap_or_default().to_owned();
        let arguments = match &call.params["arguments"] {
            Value::Null => json!({}),
            arguments => arguments.clone(),
        };
        match find(&name) {
            Some(tool) if outside && tool.kind.asks(&self.settings.ask_before) => {
                // A window that cannot be found is the tool's to report.
                match self.tool_window(arguments["window"].as_u64()) {
                    Ok(id) => self.ask_to_run(id, agent, tool, arguments, Answer(call)),
                    Err(_) => tool.start(self, arguments, Answer(call)),
                }
            }
            Some(tool) => tool.start(self, arguments, Answer(call)),
            None => {
                call.reply(Err(Error::new(
                    code::INVALID_PARAMS,
                    format!("prev has no tool {name}"),
                )));
                Task::none()
            }
        }
    }

    /// The window a tool acts on: the one numbered `window`, else the one
    /// in front.
    pub(super) fn tool_window(&self, window: Option<u64>) -> Result<window::Id, Error> {
        let found = match window {
            Some(wanted) => self
                .windows
                .keys()
                .copied()
                .find(|id| number(*id) == wanted),
            None => self
                .focused
                .filter(|id| self.windows.contains_key(id))
                .or_else(|| self.windows.keys().next().copied()),
        };
        found.ok_or_else(|| {
            Error::new(
                code::INVALID_PARAMS,
                match window {
                    Some(wanted) => format!("prev has no window {wanted}; see list_windows"),
                    None => "prev has no window open".to_owned(),
                },
            )
        })
    }

    fn window_list(&self) -> Vec<Value> {
        self.windows
            .iter()
            .map(|(id, window)| {
                let (kind, file) = match &window.content {
                    Content::Start => ("start", None),
                    Content::Document(super::Document {
                        images: Some(images),
                        ..
                    }) => {
                        let svg = matches!(
                            images.shown_image().map(|image| image.pixels),
                            Some(prev::image::window::Pixels::Svg(_))
                        );
                        let kind = if svg { "svg" } else { "image" };
                        (kind, Some(images.current_path().display().to_string()))
                    }
                    Content::Document(document) => {
                        let kind = match &document.kind {
                            Ok(Some(FileKind::Pdf)) => "pdf",
                            Ok(Some(FileKind::Image(_))) => "image",
                            Ok(Some(FileKind::Svg)) => "svg",
                            Ok(Some(FileKind::Markdown)) => "markdown",
                            Ok(None) | Err(_) => "unreadable",
                        };
                        let path = document
                            .images
                            .as_ref()
                            .map_or(document.path.as_path(), |images| images.current_path());
                        (kind, Some(path.display().to_string()))
                    }
                };
                json!({
                    "window": number(*id),
                    "title": self.base_title(*id),
                    "kind": kind,
                    "file": file,
                    "focused": self.focused == Some(*id),
                    "size": [window.size.width.round(), window.size.height.round()],
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_leave_out_what_only_a_validator_needs() {
        for tool in TOOLS.iter() {
            let schema = (tool.schema)();
            let text = schema.to_string();
            for left_out in [
                "\"$defs\"",
                "\"$ref\"",
                "\"anyOf\"",
                "\"format\"",
                "\"null\"",
                "\\n",
            ] {
                assert!(
                    !text.contains(left_out),
                    "{}: {left_out} in {text}",
                    tool.name
                );
            }
        }
        // An optional enum keeps its choices where it is used.
        let highlight = (find("highlight").expect("highlight").schema)();
        assert_eq!(highlight["properties"]["style"]["enum"][0], "highlight");
        assert_eq!(highlight["properties"]["page"]["type"], "integer");
    }

    #[test]
    fn each_window_gets_the_tools_for_its_file() {
        use read::Shows;
        let names: Vec<String> = specs(&settings::AskBefore::never())
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let kept = |shows, several| -> Vec<&str> {
            names
                .iter()
                .filter(|name| fits(name, shows, several))
                .map(String::as_str)
                .collect()
        };
        let pdf = kept(Shows::Pdf, false);
        assert!(pdf.contains(&"page_text") && pdf.contains(&"highlight"));
        assert!(!pdf.contains(&"adjust_color") && !pdf.contains(&"list_windows"));
        let image = kept(Shows::Image, false);
        assert!(image.contains(&"adjust_color") && image.contains(&"add_shape"));
        assert!(!image.contains(&"page_text") && !image.contains(&"delete_pages"));
        let markdown = kept(Shows::Markdown, false);
        assert!(markdown.contains(&"search") && !markdown.contains(&"add_shape"));
        // More windows bring the tools to choose between them.
        assert!(kept(Shows::Pdf, true).contains(&"list_windows"));
        // A window without a file yet keeps all but the window tools.
        assert_eq!(kept(Shows::Start, false).len(), names.len() - 2);
    }

    #[test]
    fn names_are_unique_and_snake_case() {
        let mut names: Vec<_> = TOOLS.iter().map(|tool| tool.name).collect();
        assert!(names.iter().all(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        }));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
    }

    #[test]
    fn every_schema_is_an_object() {
        for tool in TOOLS.iter() {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}: {schema}", tool.name);
            assert!(schema["properties"].is_object(), "{}: {schema}", tool.name);
            assert!(schema.get("$schema").is_none(), "{}", tool.name);
        }
    }

    #[test]
    fn window_input_is_described() {
        let schema = (find("focus_window").unwrap().schema)();
        let window = &schema["properties"]["window"];
        assert!(
            window["description"]
                .as_str()
                .is_some_and(|text| text.contains("list_windows")),
            "{schema}"
        );
    }

    #[test]
    fn list_has_what_mcp_needs() {
        let list = list(&settings::AskBefore::default());
        for tool in list.as_array().unwrap() {
            for field in ["name", "title", "description", "inputSchema", "annotations"] {
                assert!(tool.get(field).is_some(), "{field} in {tool}");
            }
        }
        assert_eq!(list[0]["annotations"]["readOnlyHint"], true);
        // Signing asks at first, so its description says the call waits.
        let described = |name: &str| {
            list.as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()["description"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(described("place_signature").contains("asks the user"));
        assert!(!described("list_windows").contains("asks the user"));
    }

    #[test]
    fn outputs_become_mcp_results() {
        let json = Output::Json(json!({ "a": 1 })).into_result();
        assert_eq!(json["structuredContent"]["a"], 1);
        assert_eq!(json["content"][0]["text"], r#"{"a":1}"#);
        let png = Output::Png(vec![1, 2, 3], "note".to_owned()).into_result();
        assert_eq!(png["content"][0]["type"], "image");
        assert_eq!(png["content"][0]["data"], "AQID");
        assert_eq!(png["content"][0]["mimeType"], "image/png");
        assert_eq!(png["content"][1]["text"], "note");
    }
}
