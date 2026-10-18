//! Untrusted-input tests: every file here is built to misbehave. Each test plants a sentinel
//! file *outside* the folder a tool should stay in, and checks that the tool never reads or
//! writes it. Nothing here touches files outside the test's own temporary folder.

use super::*;

/// Builds a fixture with Python's standard library (`-I`: no site packages, no cwd on the path).
fn py(script: &str, args: &[&Path]) {
    let ok = Command::new("python3").arg("-I").arg("-c").arg(script).args(args).status();
    assert!(ok.is_ok_and(|s| s.success()), "python fixture failed");
}

const ZIP_WITH_LINK: &str = r#"
import sys, zipfile, stat
out, target = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w') as z:
    z.writestr('ok.txt', 'ok')
    i = zipfile.ZipInfo('leak'); i.create_system = 3; i.external_attr = (stat.S_IFLNK | 0o777) << 16
    z.writestr(i, target)
"#;

const TAR_WITH_LINK: &str = r#"
import sys, tarfile, io
out, target = sys.argv[1], sys.argv[2]
with tarfile.open(out, 'w') as t:
    i = tarfile.TarInfo('ok.txt'); i.size = 2; t.addfile(i, io.BytesIO(b'ok'))
    i = tarfile.TarInfo('leak'); i.type = tarfile.SYMTYPE; i.linkname = target; t.addfile(i)
"#;

const TAR_WITH_DOTDOT: &str = r#"
import sys, tarfile, io
out = sys.argv[1]
with tarfile.open(out, 'w') as t:
    i = tarfile.TarInfo('ok.txt'); i.size = 2; t.addfile(i, io.BytesIO(b'ok'))
    i = tarfile.TarInfo('../escaped.txt'); i.size = 1; t.addfile(i, io.BytesIO(b'x'))
"#;

const ZIP_WITH_DOTDOT: &str = r#"
import sys, zipfile
with zipfile.ZipFile(sys.argv[1], 'w') as z:
    z.writestr('ok.txt', 'ok')
    z.writestr('../escaped.txt', 'x')
"#;

/// `home/secret.txt` (the file an archive must never reach) and `in/` (where inputs live).
fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let input = root.path().join("in");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&input).unwrap();
    let secret = home.join("secret.txt");
    std::fs::write(&secret, "SENTINEL").unwrap();
    (root, secret, input)
}

/// Every symlink under `dir`, without following any.
fn links_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            found.push(path);
        } else if meta.is_dir() {
            found.extend(links_under(&path));
        }
    }
    found
}

#[test]
fn extracting_drops_links_that_point_outside() {
    for (name, script) in [("a.zip", ZIP_WITH_LINK), ("a.tar", TAR_WITH_LINK)] {
        let (_root, secret, input) = sandbox();
        let archive = input.join(name);
        py(script, &[&archive, &secret]);
        let out = run(Op::Tool(Tool::Extract), std::slice::from_ref(&archive), None, None).unwrap();
        assert!(out[0].join("ok.txt").is_file(), "{name}: the safe entry is extracted");
        assert!(links_under(&out[0]).is_empty(), "{name}: a link out of the folder survived");
    }
}

#[test]
fn repacking_never_copies_a_linked_file_in() {
    let (_root, secret, input) = sandbox();
    let archive = input.join("a.tar");
    py(TAR_WITH_LINK, &[&archive, &secret]);
    let out = run(Op::Convert("zip"), std::slice::from_ref(&archive), None, None).unwrap();
    let listing = Command::new("unzip").arg("-Z1").arg(&out[0]).output().unwrap();
    let names = String::from_utf8_lossy(&listing.stdout);
    assert!(names.lines().any(|l| l == "ok.txt"), "{names}");
    let leaked = Command::new("unzip").arg("-p").arg(&out[0]).arg("leak").output().unwrap();
    assert!(!String::from_utf8_lossy(&leaked.stdout).contains("SENTINEL"), "the linked file was packed");
}

#[test]
fn entries_climbing_out_are_refused() {
    for (name, script) in [("a.zip", ZIP_WITH_DOTDOT), ("a.tar", TAR_WITH_DOTDOT)] {
        let (root, _secret, input) = sandbox();
        let archive = input.join(name);
        py(script, &[&archive]);
        for op in [Op::Tool(Tool::Extract), Op::Convert(if name == "a.zip" { "tar" } else { "zip" })] {
            assert!(run(op, std::slice::from_ref(&archive), None, None).is_err(), "{name} {op:?}");
            assert!(!root.path().join("escaped.txt").exists() && !input.join("escaped.txt").exists());
            // A failed job leaves nothing behind.
            let left: Vec<_> = std::fs::read_dir(&input).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert_eq!(left, vec![std::ffi::OsString::from(name)], "{name} {op:?}");
        }
    }
}

#[test]
fn entry_names_are_checked() {
    for bad in ["/etc/x", "../x", "a/../../x", "a/..", "..\\x", "a\\..\\..\\x"] {
        assert!(unsafe_entry(bad), "{bad}");
    }
    for good in ["a", "a/b/", "a..b", "..a/b", "./a", "a/./b"] {
        assert!(!unsafe_entry(good), "{good}");
    }
}

#[test]
fn seven_zip_links_never_survive() {
    if !has_program("7z") {
        return;
    }
    let (root, secret, input) = sandbox();
    let src = root.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("ok.txt"), "ok").unwrap();
    std::os::unix::fs::symlink(&secret, src.join("leak")).unwrap();
    let archive = input.join("a.7z");
    let made = Command::new("7z").args(["a", "-snl", "-bd"]).arg(&archive).arg(".").current_dir(&src).output();
    assert!(made.is_ok_and(|o| o.status.success()));
    // 7-Zip may refuse the link itself (a failed job); either way no link out is left behind.
    if let Ok(out) = run(Op::Tool(Tool::Extract), std::slice::from_ref(&archive), None, None) {
        assert!(links_under(&out[0]).is_empty());
    }
    assert_eq!(std::fs::read_dir(&input).unwrap().count(), 1 + run_outputs(&input));
}

/// Folders a successful extraction left in `dir`.
fn run_outputs(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().filter(|e| e.as_ref().unwrap().path().is_dir()).count()
}

#[test]
fn image_content_cannot_choose_the_decoder() {
    // An SVG named .png: ImageMagick sniffs the content and would run its SVG/MSVG parser.
    let (_root, _secret, input) = sandbox();
    let fake = input.join("photo.png");
    std::fs::write(&fake, r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="red"/></svg>"#).unwrap();
    assert!(run(Op::Convert("jpg"), &[fake], None, None).is_err());
}

#[test]
fn svg_cannot_read_local_files_through_coders() {
    let (_root, secret, input) = sandbox();
    let svg = |href: &str| {
        format!(
            r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="64" height="32"><rect width="64" height="32" fill="white"/>{href}</svg>"#
        )
    };
    let image = format!(r#"<image x="0" y="0" width="64" height="32" xlink:href="text:{}"/>"#, secret.display());
    std::fs::write(input.join("evil.svg"), svg(&image)).unwrap();
    std::fs::write(input.join("plain.svg"), svg("")).unwrap();
    let render = |name: &str| {
        run(Op::Convert("bmp"), &[input.join(name)], None, None).ok().map(|o| std::fs::read(&o[0]).unwrap())
    };
    let plain = render("plain.svg").expect("a plain SVG converts");
    if let Some(evil) = render("evil.svg") {
        assert_eq!(evil, plain, "the SVG drew the text of a local file");
    }
}

#[test]
fn magick_refuses_script_and_text_coders() {
    let (_root, secret, input) = sandbox();
    for source in [format!("text:{}", secret.display()), format!("msl:{}", secret.display())] {
        let out = input.join("out.png");
        let cmd = Cmd::new(magick()).arg(source.clone()).arg(&out);
        assert!(run_cmd(&cmd).is_err() && !out.exists(), "{source} was read");
    }
}
