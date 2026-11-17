//! `version new` (plan step A5b): both files made, the re-run rules after
//! each crash point, the original never written, and the refusals.

use std::path::{Path, PathBuf};

use scriptorium_core::{ParaId, Pos, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::versions::{Made, find_pair, version_new};
use scriptorium_review::sessionfile;
use scriptorium_versions::links::{self, Stamp};

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am go to the town tomorrow.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

/// A fresh folder holding `film.fdx`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-version-new-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        Self(dir.canonicalize().unwrap())
    }
    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn script(&self) -> PathBuf {
        self.at("film.fdx")
    }
    fn version(&self) -> PathBuf {
        self.at("film.es.fdx")
    }
    fn links(&self) -> PathBuf {
        self.at("film.es.links.txt")
    }
    fn make(&self) -> Result<scriptorium_fsio::versions::NewVersion, String> {
        version_new(&self.script(), "es", Some("en"), None, &mut vec![])
    }
    fn read(&self, p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn both_files_made_and_the_original_untouched() {
    let f = Folder::new("made");
    let v = f.make().unwrap();
    assert_eq!(v.made, Made::New);
    assert_eq!(v.version, f.version());
    assert!(v.version.is_absolute());
    assert_eq!(v.links, f.links());
    assert_eq!(f.read(&f.script()), SCRIPT, "the original is never written");
    assert!(v.summary().contains("film.es.fdx"), "{}", v.summary());

    let pair = links::parse(&f.read(&f.links())).unwrap();
    assert_eq!(pair.members[0].file, "film.fdx");
    assert_eq!(pair.members[0].language.as_deref(), Some("en"));
    assert_eq!(pair.members[0].hash, sessionfile::hash(SCRIPT));
    assert_eq!(pair.members[1].file, "film.es.fdx");
    assert_eq!(pair.members[1].language.as_deref(), Some("es"));
    assert_eq!(pair.members[1].hash, sessionfile::hash(&f.read(&f.version())));
    assert_eq!(pair.made(), Some(1));
    assert_eq!(pair.rows.len(), 4);
    assert_eq!(v.rows, 4);
    // The heading and the action wait for a translation; the cue doesn't;
    // the dialogue does.
    assert_eq!(v.to_translate, 3);
    assert!(matches!(pair.paras[1][&ParaId(2)].stamp, Some(Stamp::Synced(_))));

    let version = FdxFile::parse(f.read(&f.version())).unwrap();
    let texts: Vec<String> = version.to_script().paragraphs.iter().map(|p| p.text()).collect();
    assert_eq!(texts, ["EXT. FARM HOUSE - NIGHT", "", "CESAR", ""]);
    assert_eq!(version.write_script(&version.to_script()).unwrap(), version.raw(), "no-op round trip");

    // Found from either file.
    assert_eq!(find_pair(&f.0, "film.fdx").pair.unwrap().member, 0);
    assert!(find_pair(&f.0, "film.es.fdx").pair.unwrap().is_made);
}

#[test]
fn a_rerun_finds_what_is_there() {
    let f = Folder::new("rerun");
    f.make().unwrap();
    let (version, links) = (f.read(&f.version()), f.read(&f.links()));

    // Both there, unchanged: nothing written.
    assert_eq!(f.make().unwrap().made, Made::Already);
    assert_eq!(f.read(&f.links()), links);

    // Crash after the .fdx, before the links file: adopted.
    std::fs::remove_file(f.links()).unwrap();
    assert_eq!(f.make().unwrap().made, Made::Adopted);
    assert_eq!(f.read(&f.version()), version);
    assert!(f.links().exists());

    // Already made: the summary has the pair's language, the next step,
    // and no tip (the pair names the original's language).
    let v = f.make().unwrap();
    assert_eq!(v.language, "Español");
    assert!(v.summary().contains("Next: scriptorium rows"), "{}", v.summary());
    assert!(!v.summary().contains("tip:"), "{}", v.summary());

    // A re-run asking for another language or original language is refused.
    let err = version_new(&f.script(), "fr", None, Some("film.es.fdx"), &mut vec![]).unwrap_err();
    assert!(err.contains("not in fr"), "{err}");
    let err = version_new(&f.script(), "es", Some("de"), None, &mut vec![]).unwrap_err();
    assert!(err.contains("not de"), "{err}");

    // The .fdx renamed: refused with the fix, the links file kept as it is.
    let links = f.read(&f.links());
    std::fs::rename(f.version(), f.at("pelicula.fdx")).unwrap();
    let err = f.make().unwrap_err();
    assert!(err.contains("film.es.fdx is missing"), "{err}");
    assert!(err.contains("edit line 6 of film.es.links.txt"), "{err}");
    assert!(err.contains("pelicula.fdx holds the bytes"), "{err}");
    assert_eq!(f.read(&f.links()), links);
    assert!(!f.version().exists());
    assert_eq!(f.read(&f.script()), SCRIPT);
}

#[test]
fn the_tip_names_from_when_it_was_not_given() {
    let f = Folder::new("tip");
    let v = version_new(&f.script(), "es", None, None, &mut vec![]).unwrap();
    assert_eq!(v.original_language, None);
    let summary = v.summary();
    assert!(summary.contains("tip: --from en"), "{summary}");
    assert!(summary.lines().last().unwrap().starts_with("Next:"), "{summary}");
}

#[test]
fn a_broken_links_file_naming_the_script_refuses() {
    let f = Folder::new("broken");
    let other = f.at("other.links.txt");
    std::fs::write(&other, "SCRIPTORIUM LINKS v1\na: film.fdx\nb: x.fdx\n---\n").unwrap();
    let err = f.make().unwrap_err();
    assert!(err.contains("other.links.txt names film.fdx but can't be read"), "{err}");
    assert!(!f.version().exists() && !f.links().exists());
}

#[test]
fn a_crash_mid_write_is_refused_not_overwritten() {
    let f = Folder::new("partial");
    std::fs::write(f.version(), "<?xml").unwrap();
    let err = f.make().unwrap_err();
    assert!(err.contains("move it away"), "{err}");
    assert_eq!(f.read(&f.version()), "<?xml");
    assert!(!f.links().exists());
}

#[test]
fn a_saved_version_or_original_is_found_again() {
    let f = Folder::new("refind");
    f.make().unwrap();
    // Final Draft saves the version with a translation typed in.
    let saved = f.read(&f.version()).replacen(
        "<Paragraph Type=\"Dialogue\">\n      <Text></Text>",
        "<Paragraph Type=\"Dialogue\">\n      <Text>Mañana voy al pueblo.</Text>",
        1,
    );
    assert_ne!(saved, f.read(&f.version()));
    std::fs::write(f.version(), &saved).unwrap();
    let v = f.make().unwrap();
    let Made::Refound(msgs) = &v.made else { panic!("{:?}", v.made) };
    assert!(msgs[0].starts_with("film.es.fdx changed outside Scriptorium"), "{msgs:?}");
    let pair = links::parse(&f.read(&f.links())).unwrap();
    assert_eq!(pair.members[1].hash, sessionfile::hash(&saved));
    assert_eq!(pair.rows.len(), 4);

    // The original gains a line: a row of its own.
    let more = SCRIPT.replace(
        "    <Paragraph Type=\"Character\">",
        "    <Paragraph Type=\"Action\">\n      <Text>A dog barks.</Text>\n    </Paragraph>\n    <Paragraph Type=\"Character\">",
    );
    std::fs::write(f.script(), &more).unwrap();
    let v = f.make().unwrap();
    let Made::Refound(msgs) = &v.made else { panic!("{:?}", v.made) };
    assert!(msgs[0].contains("1 new"), "{msgs:?}");
    let pair = links::parse(&f.read(&f.links())).unwrap();
    assert_eq!(pair.members[0].hash, sessionfile::hash(&more));
    assert_eq!(pair.rows.len(), 5);
    assert_eq!(f.make().unwrap().made, Made::Already);
}

#[test]
fn refused_with_changes_in_the_originals_session() {
    let f = Folder::new("session");
    let mut o = Opened::open(&f.script().to_string_lossy(), &[], &mut vec![]).unwrap();
    let laura = Selection::cursor(Pos::new(ParaId(1), "Laura smiles".len()));
    o.doc.insert_text(laura, " and waves").unwrap();
    o.save().unwrap();
    let err = f.make().unwrap_err();
    assert!(err.contains("changes in Scriptorium that aren't in the file"), "{err}");
    assert!(!f.version().exists() && !f.links().exists());

    // Undone: nothing the file doesn't have.
    let mut o = Opened::open(&f.script().to_string_lossy(), &[], &mut vec![]).unwrap();
    o.doc.undo();
    o.save().unwrap();
    assert_eq!(f.make().unwrap().made, Made::New);
}

#[test]
fn refused_when_a_pair_exists() {
    let f = Folder::new("pair");
    f.make().unwrap();
    let err = version_new(&f.script(), "fr", None, None, &mut vec![]).unwrap_err();
    assert!(err.contains("already lined up in film.es.links.txt"), "{err}");
    assert!(!f.at("film.fr.fdx").exists());

    // Nor may a version be made of the version.
    let err = version_new(&f.version(), "fr", None, None, &mut vec![]).unwrap_err();
    assert!(err.contains("already lined up"), "{err}");
}

#[test]
fn a_links_file_that_is_not_ours_is_left_alone() {
    let f = Folder::new("foreign");
    std::fs::write(f.links(), "not a links file\n").unwrap();
    let err = f.make().unwrap_err();
    assert!(err.contains("can't be read"), "{err}");
    assert_eq!(f.read(&f.links()), "not a links file\n");
    assert!(!f.version().exists());
}

#[test]
fn names_and_codes_are_checked() {
    let f = Folder::new("names");
    let bad = [
        ("es", Some("../x.fdx")),
        ("es", Some("film.fdx")),
        ("es", Some("x.txt")),
        ("e/s", None),
        ("es", Some("a\nb: c.fdx")),
        ("e\ns", None),
    ];
    for (lang, name) in bad {
        assert!(version_new(&f.script(), lang, None, name, &mut vec![]).is_err(), "{lang} {name:?}");
    }
    let made: Vec<String> = std::fs::read_dir(&f.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "film.fdx")
        .collect();
    assert!(made.is_empty(), "nothing written for bad names: {made:?}");
    let v = version_new(&f.script(), "pt-BR", None, Some("Filme.fdx"), &mut vec![]).unwrap();
    assert_eq!(v.version, f.at("Filme.fdx"));
    assert_eq!(v.links, f.at("Filme.links.txt"));
}

#[test]
fn the_real_script() {
    let real = std::env::var("SCRIPTORIUM_REAL_FDX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
    let Ok(raw) = std::fs::read_to_string(&real) else {
        eprintln!("skipped: {} not found", real.display());
        return;
    };
    let f = Folder::new("real");
    std::fs::write(f.script(), &raw).unwrap();
    let v = f.make().unwrap();
    let version = FdxFile::parse(f.read(&v.version)).unwrap();
    assert_eq!(version.to_script().paragraphs.len(), FdxFile::parse(raw.clone()).unwrap().to_script().paragraphs.len());
    assert_eq!(f.make().unwrap().made, Made::Already);
    assert_eq!(f.read(&f.script()), raw);
}

#[test]
fn a_rerun_finds_again_once_and_says_it_once() {
    let f = Folder::new("rerun-once");
    f.make().unwrap();
    let saved = f.read(&f.version()).replacen(
        "<Paragraph Type=\"Dialogue\">\n      <Text></Text>",
        "<Paragraph Type=\"Dialogue\">\n      <Text>Mañana voy al pueblo.</Text>",
        1,
    );
    std::fs::write(f.version(), &saved).unwrap();
    // Opening the original re-finds (taking and releasing the links lock),
    // then `version new` takes the lock again: no deadlock, one message,
    // in the result and not also in the notes.
    let mut notes = vec![];
    let v = version_new(&f.script(), "es", Some("en"), None, &mut notes).unwrap();
    let Made::Refound(msgs) = &v.made else { panic!("{:?}", v.made) };
    assert_eq!(msgs.len(), 1, "{msgs:?}");
    assert!(msgs[0].starts_with("film.es.fdx changed outside Scriptorium"), "{msgs:?}");
    assert!(notes.is_empty(), "{notes:?}");
    // And nothing still holds the links lock.
    let lock = f.at("film.es.links.lock");
    assert!(scriptorium_fsio::SessionLock::acquire(&lock, std::time::Duration::ZERO).is_ok());
}

#[test]
fn the_pair_unusable_at_open_is_said_once_and_not_as_a_refind() {
    let f = Folder::new("unusable-once");
    f.make().unwrap();
    std::fs::rename(f.version(), f.at("pelicula.fdx")).unwrap();
    let mut notes = vec![];
    let err = version_new(&f.script(), "es", Some("en"), None, &mut notes).unwrap_err();
    assert!(err.contains("film.es.fdx is missing"), "{err}");
    // Opening the original found the pair unusable for the same reason:
    // not repeated as a note.
    assert!(notes.iter().all(|n| !n.contains("is missing")), "{notes:?}");
}
