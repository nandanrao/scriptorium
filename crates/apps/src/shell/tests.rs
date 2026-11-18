//! The shell on temporary folders: the list, pages, data, stamps, and the
//! link rule.

use super::*;

/// A fresh folder for one test (the script's folder).
fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scriptorium-apps-shell-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

const MANIFEST: &str = r#"{"scriptorium": 1, "title": "Scene list", "description": "Every scene."}"#;

/// `apps/<name>/` with a manifest and `index.html`.
fn app(dir: &Path, name: &str) -> PathBuf {
    let a = dir.join("apps").join(name);
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join("app.json"), MANIFEST).unwrap();
    fs::write(a.join("index.html"), "<p>hi</p>").unwrap();
    a
}

fn parts(p: &str) -> Vec<Part> {
    p.split('/').map(String::from).collect()
}

fn is_refused<T: fmt::Debug>(r: Result<T, FileError>) -> bool {
    matches!(r, Err(FileError::Refused(_)))
}

#[test]
fn scan_lists_sorted_with_problems() {
    let dir = temp("scan");
    app(&dir, "scene-list");
    let broken = dir.join("apps/broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("app.json"), r#"{"scriptorium": 2, "title": ""}"#).unwrap();
    let bad = dir.join("apps/Bad_Name");
    fs::create_dir_all(&bad).unwrap();
    fs::create_dir_all(dir.join("apps/.hidden")).unwrap();
    fs::write(dir.join("apps/stray.txt"), "x").unwrap();

    let found = scan(&dir);
    let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["Bad_Name", "broken", "scene-list"]);
    assert!(matches!(found[0].problems[0], Problem::BadName(_)));
    assert!(found[0].problems.contains(&Problem::NoManifest));
    assert_eq!(
        found[1].problems,
        [Problem::WrongFormat("2".into()), Problem::Empty("title"), Problem::Missing("description"), Problem::NoIndex]
    );
    assert!(found[1].manifest.is_none());
    assert!(found[2].ok(), "{:?}", found[2]);
    assert_eq!(found[2].folder, dir.join("apps/scene-list"));
    assert_eq!(found[2].manifest.as_ref().unwrap().title, "Scene list");
}

#[test]
fn scan_bad_json_and_no_apps() {
    let dir = temp("scan-json");
    assert!(scan(&dir).is_empty());
    let a = app(&dir, "x");
    fs::write(a.join("app.json"), "{nope").unwrap();
    let found = scan(&dir);
    assert!(matches!(found[0].problems[..], [Problem::BadJson(_)]), "{:?}", found[0].problems);
}

#[test]
fn scan_keeps_at_most_fifty() {
    let dir = temp("scan-many");
    for i in 0..55 {
        fs::create_dir_all(dir.join(format!("apps/a{i:02}"))).unwrap();
    }
    let found = scan(&dir);
    assert_eq!(found.len(), APPS_MAX);
    assert_eq!(found[49].name, "a49");
}

#[test]
fn pages() {
    let dir = temp("pages");
    let a = app(&dir, "scene-list");
    fs::create_dir_all(a.join("vendor")).unwrap();
    fs::write(a.join("vendor/lib.js"), "1").unwrap();
    fs::create_dir_all(a.join("data")).unwrap();
    fs::write(a.join("data/notes.json"), "{}").unwrap();

    assert_eq!(read_static(&dir, "scene-list", &[]).unwrap(), b"<p>hi</p>");
    assert_eq!(read_static(&dir, "scene-list", &parts("vendor/lib.js")).unwrap(), b"1");
    assert!(is_refused(read_static(&dir, "scene-list", &parts("data/notes.json"))));
    assert!(is_refused(read_static(&dir, "scene-list", &parts("DATA/notes.json"))));
    assert!(is_refused(read_static(&dir, "scene-list", &parts("../../x"))));
    assert!(is_refused(read_static(&dir, "scene-list", &parts(".git/config"))));
    assert!(matches!(read_static(&dir, "scene-list", &parts("vendor")), Err(FileError::NotFound)));
    assert!(matches!(read_static(&dir, "scene-list", &parts("nope.js")), Err(FileError::NotFound)));
    assert!(matches!(read_static(&dir, "other", &[]), Err(FileError::NotFound)));
    assert!(matches!(read_static(&dir, "..", &[]), Err(FileError::NotFound)));
}

#[test]
fn data_round_trip() {
    let dir = temp("data");
    app(&dir, "scene-list");
    assert_eq!(data_list(&dir, "scene-list").unwrap(), []);
    assert!(matches!(data_read(&dir, "scene-list", &parts("notes.json")), Err(FileError::NotFound)));

    data_write(&dir, "scene-list", &parts("notes.json"), b"[1]").unwrap();
    data_write(&dir, "scene-list", &parts("img/a/one.png"), b"png").unwrap();
    data_write(&dir, "scene-list", &parts("notes.json"), b"[1,2]").unwrap();
    assert_eq!(data_read(&dir, "scene-list", &parts("notes.json")).unwrap(), b"[1,2]");
    assert_eq!(data_read(&dir, "scene-list", &parts("img/a/one.png")).unwrap(), b"png");
    assert_eq!(
        data_list(&dir, "scene-list").unwrap(),
        [DataFile { path: "img/a/one.png".into(), size: 3 }, DataFile { path: "notes.json".into(), size: 5 }]
    );

    // A folder is neither written over, read nor removed.
    assert!(matches!(data_write(&dir, "scene-list", &parts("img"), b"x"), Err(FileError::NotFound)));
    assert!(matches!(data_read(&dir, "scene-list", &parts("img")), Err(FileError::NotFound)));
    assert!(matches!(data_remove(&dir, "scene-list", &parts("img")), Err(FileError::NotFound)));
    // A file in the way of a folder.
    assert!(matches!(data_write(&dir, "scene-list", &parts("notes.json/x"), b"x"), Err(FileError::NotFound)));

    data_remove(&dir, "scene-list", &parts("notes.json")).unwrap();
    assert!(matches!(data_remove(&dir, "scene-list", &parts("notes.json")), Err(FileError::NotFound)));
    assert_eq!(data_list(&dir, "scene-list").unwrap().len(), 1);

    // No app folder: nothing made.
    assert!(matches!(data_write(&dir, "ghost", &parts("a.json"), b"1"), Err(FileError::NotFound)));
    assert!(!dir.join("apps/ghost").exists());
    assert!(is_refused(data_write(&dir, "scene-list", &parts("../x.json"), b"1")));
    assert!(is_refused(data_write(&dir, "scene-list", &parts(".x.json"), b"1")));
}

#[test]
fn every_part_checked_before_anything_is_made() {
    let dir = temp("parts");
    app(&dir, "scene-list");
    let stray = dir.join("stray");
    let abs = stray.to_str().unwrap().to_string();
    for bad in [abs.as_str(), ".git", "a:b", "a\\b", "..", ""] {
        // The bad part after one that doesn't exist yet, and first.
        for p in [vec!["new".to_string(), bad.to_string(), "x.json".into()], vec![bad.to_string()]] {
            assert!(is_refused(data_write(&dir, "scene-list", &p, b"1")), "{p:?}");
            assert!(is_refused(data_read(&dir, "scene-list", &p)), "{p:?}");
            assert!(is_refused(data_remove(&dir, "scene-list", &p)), "{p:?}");
            assert!(is_refused(read_static(&dir, "scene-list", &p)), "{p:?}");
        }
    }
    assert!(!stray.exists(), "no stray folder made");
    assert!(!dir.join("apps/scene-list/data").exists(), "nothing made");
}

#[test]
fn stamps_are_bounded() {
    let dir = temp("stamp-bounds");
    let a = app(&dir, "a");
    // Deeper than DEPTH_MAX: not walked.
    let deep = (0..=DEPTH_MAX).fold(a.clone(), |p, i| p.join(format!("d{i}")));
    fs::create_dir_all(&deep).unwrap();
    let before = stamp(&dir);
    fs::write(deep.join("x.js"), "x").unwrap();
    assert_eq!(stamp(&dir), before, "a file past the depth limit isn't stamped");
    // The cap is shared by every app: with it spent, later apps stamp no files.
    let mut rows = Vec::new();
    walk_files(&a, "", true, 0, 1, &mut rows);
    assert_eq!(rows.len(), 1);
}

#[test]
fn over_twenty_megabytes_refused_with_nothing_left() {
    let dir = temp("big");
    app(&dir, "scene-list");
    let big = vec![b'x'; DATA_MAX as usize + 1];
    assert!(matches!(data_write(&dir, "scene-list", &parts("img/big.bin"), &big), Err(FileError::TooLarge)));
    assert!(!dir.join("apps/scene-list/data").exists());
    data_write(&dir, "scene-list", &parts("ok.bin"), &big[..DATA_MAX as usize]).unwrap();
    assert!(matches!(data_write(&dir, "scene-list", &parts("ok.bin"), &big), Err(FileError::TooLarge)));
    assert_eq!(fs::metadata(dir.join("apps/scene-list/data/ok.bin")).unwrap().len(), DATA_MAX);
    let names: Vec<_> =
        fs::read_dir(dir.join("apps/scene-list/data")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["ok.bin"], "no temporary file left");
}

#[test]
fn stamps_follow_the_files_not_data() {
    let dir = temp("stamp");
    assert!(stamp(&dir).is_empty());
    let a = app(&dir, "scene-list");
    app(&dir, "other");
    let first = stamp(&dir);
    assert_eq!(first.keys().collect::<Vec<_>>(), ["other", "scene-list"]);
    assert_eq!(stamp(&dir), first, "nothing changed, same stamps");

    fs::create_dir_all(a.join("data")).unwrap();
    fs::write(a.join("data/notes.json"), "{}").unwrap();
    fs::write(a.join(".index.html.swp"), "x").unwrap();
    assert_eq!(stamp(&dir), first, "data/ and hidden files don't count");

    fs::write(a.join("app.js"), "x").unwrap();
    let second = stamp(&dir);
    assert_ne!(second["scene-list"], first["scene-list"]);
    assert_eq!(second["other"], first["other"]);
    fs::create_dir_all(a.join("vendor")).unwrap();
    fs::write(a.join("vendor/lib.js"), "y").unwrap();
    assert_ne!(stamp(&dir)["scene-list"], second["scene-list"], "files in subfolders count");
}

#[cfg(unix)]
mod links {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn data_itself_a_link_out() {
        let dir = temp("link-data");
        let outside = temp("link-data-outside");
        fs::write(outside.join("secret.txt"), "s").unwrap();
        let a = app(&dir, "scene-list");
        symlink(&outside, a.join("data")).unwrap();

        assert!(is_refused(data_write(&dir, "scene-list", &parts("x.json"), b"1")));
        assert!(is_refused(data_write(&dir, "scene-list", &parts("new/x.json"), b"1")));
        assert!(is_refused(data_read(&dir, "scene-list", &parts("secret.txt"))));
        assert!(is_refused(data_remove(&dir, "scene-list", &parts("secret.txt"))));
        assert!(is_refused(data_list(&dir, "scene-list")));
        let left: Vec<_> = fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["secret.txt"], "nothing written or removed outside");
    }

    #[test]
    fn a_middle_folder_a_link() {
        let dir = temp("link-middle");
        let outside = temp("link-middle-outside");
        fs::write(outside.join("a.png"), "s").unwrap();
        let a = app(&dir, "scene-list");
        fs::create_dir_all(a.join("data")).unwrap();
        symlink(&outside, a.join("data/img")).unwrap();

        assert!(is_refused(data_write(&dir, "scene-list", &parts("img/b.png"), b"1")));
        assert!(is_refused(data_write(&dir, "scene-list", &parts("img/sub/b.png"), b"1")));
        assert!(is_refused(data_read(&dir, "scene-list", &parts("img/a.png"))));
        assert!(is_refused(data_remove(&dir, "scene-list", &parts("img/a.png"))));
        assert_eq!(data_list(&dir, "scene-list").unwrap(), [], "the link isn't listed");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);

        // A page's folder linked out, too.
        symlink(&outside, a.join("vendor")).unwrap();
        assert!(is_refused(read_static(&dir, "scene-list", &parts("vendor/a.png"))));
    }

    #[test]
    fn apps_a_link() {
        let dir = temp("link-apps");
        let elsewhere = temp("link-apps-elsewhere");
        app(&elsewhere, "scene-list");
        symlink(elsewhere.join("apps"), dir.join("apps")).unwrap();

        assert!(scan(&dir).is_empty());
        assert!(stamp(&dir).is_empty());
        assert!(is_refused(read_static(&dir, "scene-list", &[])));
        assert!(is_refused(data_write(&dir, "scene-list", &parts("x.json"), b"1")));
        assert!(!elsewhere.join("apps/scene-list/data").exists());
    }

    #[test]
    fn an_app_folder_a_link() {
        let dir = temp("link-app");
        let elsewhere = temp("link-app-elsewhere");
        let real = app(&elsewhere, "scene-list");
        app(&dir, "mine");
        symlink(&real, dir.join("apps/scene-list")).unwrap();

        let names: Vec<_> = scan(&dir).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["mine"]);
        assert_eq!(stamp(&dir).keys().collect::<Vec<_>>(), ["mine"]);
        assert!(is_refused(read_static(&dir, "scene-list", &[])));
        assert!(is_refused(data_write(&dir, "scene-list", &parts("x.json"), b"1")));
    }

    #[test]
    fn the_file_a_link() {
        let dir = temp("link-file");
        let outside = temp("link-file-outside");
        fs::write(outside.join("secret.txt"), "s").unwrap();
        let a = app(&dir, "scene-list");
        fs::create_dir_all(a.join("data")).unwrap();
        symlink(outside.join("secret.txt"), a.join("data/notes.json")).unwrap();
        symlink(outside.join("secret.txt"), a.join("page.html")).unwrap();

        assert!(is_refused(data_read(&dir, "scene-list", &parts("notes.json"))));
        assert!(is_refused(data_write(&dir, "scene-list", &parts("notes.json"), b"1")));
        assert!(is_refused(data_remove(&dir, "scene-list", &parts("notes.json"))));
        assert!(is_refused(read_static(&dir, "scene-list", &parts("page.html"))));
        assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"s");
        assert!(fs::symlink_metadata(a.join("data/notes.json")).unwrap().file_type().is_symlink());

        // A linked manifest or entry page counts as missing.
        fs::remove_file(a.join("index.html")).unwrap();
        symlink(outside.join("secret.txt"), a.join("index.html")).unwrap();
        assert!(scan(&dir)[0].problems.contains(&Problem::NoIndex));
    }
}
