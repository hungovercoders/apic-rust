//! Regenerates the terminal screenshots in the docs assets from real apic
//! output: it serves the demo API in-process, runs the requests the docs
//! talk about, and renders the frames the CLI and the UI produce as SVG.
//! Run it with `task shots` after changing anything on screen.

#![forbid(unsafe_code)]

mod svg;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use apic_gocompat::{mkdir_all, path_error, FlagSet};
use apic_httpc::CancelToken;
use apic_httpfile::Request;
use apic_output::style::{set_color_profile, Profile};
use apic_output::Theme;
use apic_project::path;
use apic_runner::{shared_session, Options, Runner};
use apic_ui::{Config, Model};

use svg::{animated_svg, Frame};

/// The port the screenshots show. It only has to be free on the machine
/// generating them; the URL in the picture is part of the picture.
const DEMO_PORT: u16 = 8089;

/// The size of the terminal in the ui screenshot, in characters.
const UI_COLS: i32 = 112;
const UI_ROWS: i32 = 19;

fn main() {
    let mut flags = FlagSet::new("shot");
    flags.string(
        "out",
        "website/public/assets",
        "directory to write the SVGs into",
    );
    flags.parse_env();
    if let Err(err) = generate(flags.get("out")) {
        eprintln!("shot: {err}");
        std::process::exit(1);
    }
}

/// One picture: the file it is written to, the title of its window and
/// what draws its frames.
struct Shot {
    file: &'static str,
    title: &'static str,
    render: fn(&str) -> Result<Vec<Frame>, String>,
}

fn generate(dir: &str) -> Result<(), String> {
    // The frames are captured off a terminal, so nothing can detect one:
    // ask for colour explicitly, the way a real session has it.
    set_color_profile(Profile::Ansi256);

    mkdir_all(Path::new(dir))?;
    let demo = start_demo()?;
    let root = demo.root();

    let shots = [
        Shot {
            file: "apic-ui.svg",
            title: "apic ui --demo",
            render: |root| single(ui_frame(root)),
        },
        Shot {
            file: "apic-run.svg",
            title: "apic run login whoami",
            render: |root| single(run_output(root)),
        },
        Shot {
            file: "apic-demo.svg",
            title: "apic ui --demo",
            render: ui_animation,
        },
    ];
    for s in &shots {
        let frames = (s.render)(&root).map_err(|err| format!("{}: {err}", s.file))?;
        let target = path::join(dir, s.file);
        write_file(&target, &animated_svg(s.title, &frames))?;
        println!("wrote {target}");
        if frames.len() < 2 {
            continue;
        }
        // The poster is the last frame on its own: what the pages show in
        // place of the animation under prefers-reduced-motion
        // (website/plugins/rehype-recordings.mjs, and the README's <picture>).
        let poster = format!(
            "{}-poster.svg",
            target.strip_suffix(".svg").unwrap_or(&target)
        );
        write_file(&poster, &animated_svg(s.title, &frames[frames.len() - 1..]))?;
        println!("wrote {poster}");
    }
    Ok(())
}

fn write_file(target: &str, content: &str) -> Result<(), String> {
    std::fs::write(target, content).map_err(|err| path_error("open", Path::new(target), &err))
}

/// Adapts a one-frame rendering to the animation shape.
fn single(text: Result<String, String>) -> Result<Vec<Frame>, String> {
    Ok(vec![Frame {
        text: text?,
        ..Frame::default()
    }])
}

/// The UI over the demo project, at the size the pictures are drawn at.
fn new_model(r: &Arc<Runner>) -> Model {
    let mut cfg = Config::new(r.clone());
    // The status bar shows the project root, and a temporary directory
    // would be different in every regeneration; name it the way `apic
    // demo` names the one it scaffolds.
    cfg.root = "apic-demo".to_string();
    cfg.env = r.opts.env.clone();
    cfg.demo = true;
    cfg.theme = Theme::default();
    let mut m = Model::new(cfg);
    m.resize(UI_COLS, UI_ROWS);
    m
}

/// Captures the screen as the next frame, or holds the last frame longer
/// when nothing changed on it.
fn add(frames: &mut Vec<Frame>, m: &mut Model, hold: Duration, caption: &str) {
    let text = m.view();
    if let Some(last) = frames.last_mut() {
        if last.text == text {
            last.hold += hold; // nothing changed on screen: hold the last frame longer
            return;
        }
    }
    frames.push(Frame {
        text,
        hold,
        caption: caption.to_string(),
    });
}

/// The moving picture on the README and the docs home: a tour of the UI in
/// about twenty seconds, every frame captured from the model after a real
/// key press, so it shows what the UI does today. The caption in the title
/// bar names the key.
fn ui_animation(root: &str) -> Result<Vec<Frame>, String> {
    let r = new_runner(root)?;
    let mut m = new_model(&r);
    let ms = Duration::from_millis;

    let mut frames = Vec::new();
    add(
        &mut frames,
        &mut m,
        ms(2000),
        "the project: requests left, preview right",
    );
    m.press("enter");
    add(&mut frames, &mut m, ms(2500), "enter · send the request");
    m.press("j");
    add(
        &mut frames,
        &mut m,
        ms(1500),
        "j · whoami is ready: login captured the token",
    );
    // Down to the first request in todos.http. The loop is bounded by the
    // project size so a renamed request fails loudly instead of spinning.
    let mut i = r.project.requests().len();
    loop {
        if m.selected().is_some_and(|sel| sel.id() == "list-todos") {
            break;
        }
        if i == 0 {
            return Err(
                "the demo project has no list-todos request to move the cursor to".to_string(),
            );
        }
        m.press("j");
        i -= 1;
    }
    add(&mut frames, &mut m, ms(1000), "j … · todos.http");
    // Run the file as a flow, one frame per request landing.
    m.press_watch("f", |m| {
        add(&mut frames, m, ms(450), "f · run the whole file, live");
    });
    if let Some(last) = frames.last_mut() {
        last.hold = ms(2000);
    }
    m.press("3");
    add(
        &mut frames,
        &mut m,
        ms(2500),
        "3 · checks: actual against expected",
    );
    m.press("4");
    add(
        &mut frames,
        &mut m,
        ms(2000),
        "4 · session: what was captured",
    );
    m.press("?");
    add(&mut frames, &mut m, ms(2500), "? · every key");
    m.press("esc");
    m.press("1");
    add(&mut frames, &mut m, ms(1500), "esc · back to it");
    Ok(frames)
}

/// Drives `apic ui` to the frame the docs lead with: everything run once
/// (the demo project has one request that fails on purpose) and the cursor
/// on list-todos, showing its response.
fn ui_frame(root: &str) -> Result<String, String> {
    let r = new_runner(root)?;
    let mut m = new_model(&r);
    // `a` runs everything, `g` goes to the top, then down past auth.http,
    // explore.http and jobs.http to the first request in todos.http.
    m.press_all("a g j j j j j j j j j j j j");
    let selected = m.selected().map(|sel| sel.id());
    if selected.as_deref() != Some("list-todos") {
        return Err(format!(
            "the ui frame should show list-todos, got {}",
            selected.as_deref().unwrap_or("<nil>")
        ));
    }
    Ok(m.view())
}

/// What `apic run login whoami` prints, under the prompt that asked for
/// it.
fn run_output(root: &str) -> Result<String, String> {
    let r = new_runner(root)?;
    let mut reqs: Vec<Arc<Request>> = Vec::new();
    for name in ["login", "whoami"] {
        reqs.extend(r.project.resolve(name).map_err(|err| err.to_string())?);
    }
    let (results, err) = r.run_all(&CancelToken::new(), &reqs);
    if let Some(err) = err {
        return Err(err.to_string());
    }
    let mut b: Vec<u8> = Vec::new();
    let t = Theme::default();
    b.extend_from_slice(format!("{} apic run login whoami\n", t.accent.render("$")).as_bytes());
    for (i, res) in results.iter().enumerate() {
        if i > 0 {
            b.push(b'\n');
        }
        apic_output::human(&mut b, res, false);
    }
    apic_output::summary(&mut b, &results);
    Ok(String::from_utf8_lossy(&b).into_owned())
}

fn new_runner(root: &str) -> Result<Arc<Runner>, String> {
    let p = apic_project::load(root).map_err(|err| err.to_string())?;
    let opts = Options {
        env: "local".to_string(),
        session: Some(shared_session(apic_session::new_memory())),
        ..Options::default()
    };
    Runner::new(p, opts)
        .map(Arc::new)
        .map_err(|err| err.to_string())
}

/// The demo project in a temporary directory and the fake API it targets,
/// both gone when this is dropped: the server first, then the directory.
struct Demo {
    _server: apic_httpd::Server,
    dir: tempfile::TempDir,
}

impl Demo {
    fn root(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }
}

/// Writes the demo project to a temporary directory and serves the fake
/// API it targets, exactly as `apic ui --demo` does.
fn start_demo() -> Result<Demo, String> {
    let dir = tempfile::Builder::new()
        .prefix("apic-shot-")
        .tempdir()
        .map_err(|err| err.to_string())?;
    let listener = apic_httpd::Listener::bind(&format!("127.0.0.1:{DEMO_PORT}"))
        .map_err(|err| format!("the screenshots show port {DEMO_PORT}, which is in use: {err}"))?;
    apic_demoapi::write_project(&dir.path().to_string_lossy(), DEMO_PORT, true)?;
    let server = apic_httpd::Server::start(listener, apic_demoapi::new());
    Ok(Demo {
        _server: server,
        dir,
    })
}
