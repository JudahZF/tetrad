//! Qt INI and `@Variant` codec contracts. All data is synthetic and shaped
//! like the stock files (same encodings, no real values).

use crate::{IniValue, SettingsFile, Variant, VariantError};

/// Stock shapes: `[General]` first, `%20` key escapes, `\`-paths, floats as
/// `@Variant` (quoted when a byte is `=`), `QList<int>` user type, a
/// `QVariantMap`, a quoted `@ByteArray`, a comma list and a blank line
/// between sections.
const CANONICAL: &[u8] = br#"[General]
brightness=40
bypassSaveMode=SaveBypassNever
selected_connector=Output 1/2

[ConnectorsSettings]
100=1
exp%20100=2
exp%20max%20100=@Variant(\0\0\0\x87?\x80\0\0)

[GlobalEq]
0\0=@Variant(\0\0\0\x87?\0\0\0)
0\1="@Variant(\0\0\0\x87=\xcc\xcc\xcd)"

[Licenses]
licenses="@ByteArray(a;b\x1\x2)"
new=@Variant(\0\0\0\x7f\0\0\0\vQList<int>\0\0\0\0\x3\0\0\0\0\0\0\0\x5\0\0\0\xf)

[Mode]
availableModes=1, 2

[TableSettings]
captureFilter=@Variant(\0\0\0\b\0\0\0\x2\0\0\0\x16\0O\0W\0N\0\x45\0\x44\0_\0\x42\0Y\0_\0M\0\x45\0\0\0\x1\0\0\0\0\x18\0\x44\0\x45\0V\0I\0\x43\0\x45\0_\0T\0Y\0P\0\x45\0S\0\0\0\x7f\0\0\0\vQList<int>\0\0\0\0\0)
"#;

#[test]
fn canonical_file_round_trips_byte_exact() {
    let (file, issues) = SettingsFile::parse(CANONICAL);
    assert!(issues.is_empty());
    assert_eq!(file.to_bytes(), CANONICAL);
    assert!(file.contains("ConnectorsSettings/exp max 100"));
    assert!(file.contains("GlobalEq/0/1"));
}

#[test]
fn stock_variant_encodings_decode_and_reencode_identically() {
    let (file, _) = SettingsFile::parse(CANONICAL);
    let value = |k: &str| file.value(k).unwrap().unwrap();
    assert_eq!(value("ConnectorsSettings/exp max 100"), Variant::Float(1.0));
    assert_eq!(value("GlobalEq/0/1"), Variant::Float(0.1));
    assert_eq!(value("Licenses/new"), Variant::IntList(vec![0, 5, 15]));
    assert_eq!(
        value("Licenses/licenses"),
        Variant::ByteArray(b"a;b\x01\x02".to_vec())
    );
    assert_eq!(value("Mode/availableModes").to_int_list(), Some(vec![1, 2]));
    let filter = Variant::map([
        ("DEVICE_TYPES".to_owned(), Variant::IntList(vec![])),
        ("OWNED_BY_ME".to_owned(), Variant::Bool(false)),
    ]);
    assert_eq!(value("TableSettings/captureFilter"), filter);
    for key in [
        "TableSettings/captureFilter",
        "Licenses/new",
        "GlobalEq/0/1",
        "Licenses/licenses",
    ] {
        assert_eq!(&value(key).to_ini(), file.get(key).unwrap(), "{key}");
    }
}

#[test]
fn text_escapes_round_trip_like_qt() {
    let cases: [(&str, &[u8]); 6] = [
        // `\0` then a hex digit: the digit is escaped so it is not octal.
        ("a\u{0}1", br"x=a\0\x31"),
        // `\xNN` then a hex digit: the digit is escaped too.
        ("\u{87}A", br"x=\x87\x41"),
        // No INI codec: every unit >= 0x7F is `\x`, surrogates one by one.
        (
            "Caf\u{e9} \u{4e2d}\u{1f3b8}",
            br"x=Caf\xe9 \x4e2d\xd83c\xdfb8",
        ),
        ("a=b", br#"x="a=b""#),
        (" pad", br#"x=" pad""#),
        ("@at", br"x=@@at"),
    ];
    for (text, line) in cases {
        let mut file = SettingsFile::new();
        file.set("x", &Variant::String(text.to_owned()));
        let bytes = file.to_bytes();
        assert_eq!(&bytes[10..bytes.len() - 1], line, "{text:?}");
        let (back, _) = SettingsFile::parse(&bytes);
        assert_eq!(
            back.value("x").unwrap().unwrap(),
            Variant::String(text.to_owned())
        );
    }
}

#[test]
fn string_lists_follow_qt_rules() {
    let mut file = SettingsFile::new();
    file.set(
        "a",
        &Variant::StringList(vec!["x".into(), "@y".into(), "".into()]),
    );
    file.set("b", &Variant::StringList(vec![]));
    file.set(
        "c",
        &Variant::List(vec![Variant::Float(1.0), Variant::Int(2)]),
    );
    let bytes = file.to_bytes();
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        "[General]\na=x, @@y, \nb=@Invalid()\nc=@Variant(\\0\\0\\0\\x87?\\x80\\0\\0), 2\n"
    );
    let (back, _) = SettingsFile::parse(&bytes);
    let a = back.value("a").unwrap().unwrap();
    assert_eq!(a.to_string_list(), ["x", "@y", ""]);
    assert!(
        back.value("b")
            .unwrap()
            .unwrap()
            .to_string_list()
            .is_empty()
    );
    assert_eq!(
        back.value("c").unwrap().unwrap(),
        Variant::List(vec![Variant::Float(1.0), Variant::String("2".into())])
    );
}

/// `writeIniFile` sorts by (int position, name): keys read from the file
/// keep their order, new keys (-1) come first. A key removed and set again
/// before a save keeps its place.
#[test]
fn write_order_follows_qt_positions() {
    let (mut file, _) = SettingsFile::parse(b"[Z]\nb=1\na=2\n\n[A]\nk=1\n");
    file.set("Z/c", &Variant::Int(3));
    file.set("Z/a", &Variant::Int(9));
    file.remove("Z/b");
    file.set("Z/b", &Variant::Int(1));
    file.set("New/x", &Variant::Bool(true));
    assert_eq!(
        String::from_utf8(file.save()).unwrap(),
        "[New]\nx=true\n\n[Z]\nc=3\nb=1\na=9\n\n[A]\nk=1\n"
    );
}

#[test]
fn parser_tolerates_hand_edits_like_qt() {
    let data = b"; comment\r\n[%General]\r\nk = \"a, b\" , c ;tail\r\nbad line\r\n[general]\nroot=1\nroot=2\n[S\nx=1\n";
    let (file, issues) = SettingsFile::parse(data);
    assert_eq!(issues.len(), 2);
    assert_eq!(
        file.get("General/k"),
        Some(&IniValue::List(vec!["a, b".into(), "c".into()]))
    );
    // Duplicate key: last value wins.
    assert_eq!(file.get("root"), Some(&IniValue::Text("2".into())));
    assert!(file.contains("S/x"));
    // Root keys keep section position 0 although `[general]` came later.
    assert_eq!(
        String::from_utf8(file.to_bytes()).unwrap(),
        "[General]\nroot=2\n\n[%General]\nk=\"a, b\", c\n\n[S]\nx=1\n"
    );
}

#[test]
fn group_queries_and_remove_match_qsettings() {
    let (mut file, _) = SettingsFile::parse(b"[G]\na=1\nsub\\b=2\nsub\\c\\d=3\n\n[H]\nx=1\n");
    assert_eq!(file.child_keys("G"), ["a"]);
    assert_eq!(file.child_groups("G"), ["sub"]);
    assert_eq!(file.child_groups(""), ["G", "H"]);
    assert_eq!(file.child_keys("/G//sub/"), ["b"]);
    file.remove("G/sub");
    assert_eq!(file.keys().collect::<Vec<_>>(), ["G/a", "H/x"]);
    assert!(file.is_dirty());
    file.save();
    assert!(!file.is_dirty());
}

#[test]
fn text_conversions_follow_qvariant() {
    let s = |v: &str| Variant::String(v.into());
    assert!(!s("False").to_bool());
    assert!(!s("0").to_bool());
    assert!(s("no").to_bool());
    assert_eq!(s(" 42 ").to_int(), Some(42));
    assert_eq!(s("4x").to_int(), None);
    assert_eq!(Variant::Float(2.5).to_int(), Some(3));
    assert_eq!(Variant::Float(-2.5).to_int(), Some(-2));
    assert_eq!(Variant::Double(0.1).to_ini(), IniValue::Text("0.1".into()));
    assert_eq!(
        Variant::Double(1e20).to_ini(),
        IniValue::Text("1e+20".into())
    );
}

/// A list nested 40 levels deep is an error, not a stack overflow.
#[test]
fn deeply_nested_variant_lists_are_rejected() {
    let decode = |depth: usize| {
        let mut payload: Vec<u8> = [0, 0, 0, 9, 0, 0, 0, 1].repeat(depth);
        payload.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 7]);
        let text: String = payload.iter().map(|&b| char::from(b)).collect();
        Variant::from_ini(&IniValue::Text(format!("@Variant({text})")))
    };
    assert!(decode(4).is_ok());
    assert_eq!(decode(40), Err(VariantError::TooDeep));
}
