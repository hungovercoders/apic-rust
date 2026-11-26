//! Replays `testdata/style_golden.json`: styles, text functions and
//! layouts with the output they must give, byte for byte, under each
//! colour profile. `testdata/golden.go` writes the file.

use serde_json::Value;

use super::*;

fn golden() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/style_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the golden file")).expect("JSON")
}

fn border_named(name: &str) -> Border {
    match name {
        "normal" => normal_border(),
        "rounded" => rounded_border(),
        "thick" => thick_border(),
        "double" => double_border(),
        "hidden" => hidden_border(),
        "block" => block_border(),
        "outer_half" => outer_half_block_border(),
        "inner_half" => inner_half_block_border(),
        "markdown" => markdown_border(),
        "ascii" => ascii_border(),
        other => panic!("unknown border {other}"),
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn ints(v: &Value) -> Vec<i32> {
    v.as_array()
        .map(|a| a.iter().map(|n| n.as_i64().unwrap() as i32).collect())
        .unwrap_or_default()
}

fn build(sp: &Value) -> Style {
    let mut s = Style::new();
    let flag = |k: &str| sp.get(k).and_then(Value::as_bool);
    let text = |k: &str| sp.get(k).and_then(Value::as_str);
    let int = |k: &str| sp.get(k).and_then(Value::as_i64).map(|n| n as i32);
    let float = |k: &str| sp.get(k).and_then(Value::as_f64);
    macro_rules! set {
        ($get:ident, $($key:literal => $method:ident),*) => {$(
            if let Some(v) = $get($key) {
                s = s.$method(v);
            }
        )*};
    }
    set!(flag,
        "bold" => bold, "italic" => italic, "underline" => underline,
        "strikethrough" => strikethrough, "reverse" => reverse, "blink" => blink,
        "faint" => faint, "underline_spaces" => underline_spaces,
        "strikethrough_spaces" => strikethrough_spaces,
        "color_whitespace" => color_whitespace, "inline" => inline);
    set!(text, "fg" => foreground, "bg" => background);
    if let Some(a) = sp.get("fg_adaptive") {
        let a = strings(a);
        s = s.foreground(Color::adaptive(&a[0], &a[1]));
    }
    if let Some(a) = sp.get("fg_complete") {
        let a = strings(a);
        s = s.foreground(Color::complete(&a[0], &a[1], &a[2]));
    }
    set!(text, "margin_bg" => margin_background);
    set!(int, "width" => width, "height" => height, "max_width" => max_width,
        "max_height" => max_height, "tab_width" => tab_width);
    if let Some(p) = float("align_h") {
        s = s.align_horizontal(Position(p));
    }
    if let Some(p) = float("align_v") {
        s = s.align_vertical(Position(p));
    }
    if let Some(p) = sp.get("padding") {
        s = s.padding(&ints(p));
    }
    if let Some(m) = sp.get("margin") {
        s = s.margin(&ints(m));
    }
    if let Some(b) = text("border") {
        if flag("border_style_only") == Some(true) {
            s = s.border_style(border_named(b));
        } else {
            let sides: Vec<bool> = sp
                .get("border_sides")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_bool().unwrap()).collect())
                .unwrap_or_default();
            s = s.border(border_named(b), &sides);
        }
    }
    set!(flag, "border_top" => border_top, "border_right" => border_right,
        "border_bottom" => border_bottom, "border_left" => border_left);
    set!(text, "border_fg" => border_foreground, "border_bg" => border_background);
    if let Some(c) = sp.get("border_fg_sides") {
        let colors: Vec<Color> = strings(c).into_iter().map(Color::from).collect();
        s = s.border_foreground_sides(&colors);
    }
    s
}

/// Tests that change the background setting, which is process-wide, take
/// this lock.
static BACKGROUND: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn styles_render_as_recorded() {
    let _guard = BACKGROUND.lock().unwrap();
    let golden = golden();
    let profiles = [
        ("truecolor", Profile::TrueColor),
        ("ansi256", Profile::Ansi256),
        ("ansi", Profile::Ansi),
        ("ascii", Profile::Ascii),
    ];
    let cases = golden["styles"].as_array().unwrap();
    assert!(cases.len() > 500, "the golden file lost its cases");
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let spec = &case["style"];
        let input = case["input"].as_str().unwrap();
        set_has_dark_background(spec.get("dark").and_then(Value::as_bool).unwrap_or(true));
        let style = build(spec);
        for (name, profile) in profiles {
            let want = case["out"][name].as_str().unwrap();
            // Through the profile in force and through the explicit one:
            // both must agree with the record.
            let got = with_color_profile(profile, || style.render(input));
            if got != want || style.render_with(profile, input) != want {
                failures.push(format!(
                    "case {i} {spec} {input:?} [{name}]\n  got  {got:?}\n  want {want:?}"
                ));
            }
        }
    }
    set_has_dark_background(true);
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len() * 4,
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn text_functions_give_what_was_recorded() {
    let golden = golden();
    let cases = golden["fns"].as_array().unwrap();
    assert!(cases.len() > 2000, "the golden file lost its cases");
    let mut failures = Vec::new();
    for case in cases {
        let s = case.get("s").and_then(Value::as_str).unwrap_or("");
        let int = |k: &str| case[k].as_i64().unwrap() as i32;
        let text = |k: &str| case.get(k).and_then(Value::as_str).unwrap_or("");
        let pos = |k: &str| Position(case[k].as_f64().unwrap());
        let blocks = strings(case.get("blocks").unwrap_or(&Value::Null));
        let blocks: Vec<&str> = blocks.iter().map(String::as_str).collect();
        let got = match case["fn"].as_str().unwrap() {
            "string_width" => Value::from(string_width(s)),
            "width" => Value::from(width(s)),
            "height" => Value::from(height(s)),
            "strip" => Value::from(strip(s)),
            "truncate" => Value::from(truncate(s, int("n"), text("tail"))),
            "truncate_left" => Value::from(truncate_left(s, int("n"), text("tail"))),
            "cut" => Value::from(cut(s, int("l"), int("r"))),
            "hardwrap" => Value::from(hardwrap(s, int("n"), case["preserve"].as_bool().unwrap())),
            "wordwrap" => Value::from(wordwrap(s, int("n"), text("bp"))),
            "wrap" => Value::from(wrap(s, int("n"), text("bp"))),
            "join_horizontal" => Value::from(join_horizontal(pos("pos"), &blocks)),
            "join_vertical" => Value::from(join_vertical(pos("pos"), &blocks)),
            "place_horizontal" => Value::from(place_horizontal(int("n"), pos("pos"), s)),
            "place_vertical" => Value::from(place_vertical(int("n"), pos("pos"), s)),
            "place" => Value::from(place(int("w"), int("h"), pos("hp"), pos("vp"), s)),
            other => panic!("unknown function {other}"),
        };
        if got != case["out"] {
            let mut spec = case.clone();
            spec.as_object_mut().unwrap().remove("out");
            failures.push(format!("{spec}\n  got  {got}\n  want {}", case["out"]));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_parser_table_is_the_recorded_one() {
    let golden = golden();
    let want = golden["parser_table"].as_str().unwrap();
    let got: String = parser::table().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, want);
}

#[test]
fn a_style_with_nothing_set_only_converts_tabs() {
    assert_eq!(
        Style::new().render_with(Profile::TrueColor, "a\tb\r\n"),
        "a    b\r\n"
    );
}

#[test]
fn frame_sizes_count_margins_borders_and_padding() {
    let s = Style::new()
        .border(rounded_border(), &[])
        .padding(&[0, 1])
        .margin(&[2, 3]);
    assert_eq!(s.horizontal_frame_size(), 2 + 2 + 6);
    assert_eq!(s.vertical_frame_size(), 2 + 4);
    assert_eq!(Style::new().bold(true).horizontal_frame_size(), 0);
}

#[test]
fn colours_give_their_parameters() {
    assert_eq!(
        Color::new("212").sequence(Profile::Ansi256, false),
        "38;5;212"
    );
    assert_eq!(Color::ansi(9).sequence(Profile::Ansi, true), "101");
    assert_eq!(
        Color::new("#ff87d7").sequence(Profile::TrueColor, false),
        "38;2;255;135;215"
    );
    assert_eq!(Color::new("212").sequence(Profile::Ascii, false), "");
    assert_eq!(Color::None.sequence(Profile::TrueColor, false), "");
    assert_eq!(Color::new("300").sequence(Profile::Ansi, false), "");
}
