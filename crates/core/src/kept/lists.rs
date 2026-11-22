use crate::base::error::Error;
use crate::said;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Saved {
    pub name: String,
    /// With what the lists it builds on give now.
    pub mods: usize,
    pub bases: Vec<String>,
    /// Every list under it, the nearest first.
    pub under: Vec<String>,
    /// How many mods it adds to and turns off from what they give.
    pub adds: usize,
    pub drops: usize,
    /// Seconds since 1970.
    pub modified: u64,
}

/// Characters a file name cannot hold become `_`: names differing only so share a file,
/// and `save` refuses the second.
fn file_for(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.json"))
}

/// What a list file holds.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Kept {
    name: String,
    /// The whole load order when it was last saved.
    ids: Vec<String>,
    /// The lists it builds on; `adds` and `drops` are what it changes from them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    adds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    drops: Vec<String>,
}

pub fn save(dir: &Path, name: &str, ids: &[String]) -> Result<(), Error> {
    if name.trim().is_empty() {
        return Err(Error::refused(said!("Give the list a name.")));
    }
    let path = file_for(dir, name);
    let old = read(&path);
    if let Some(other) = old.as_ref().filter(|k| k.name != name.trim()) {
        return Err(Error::refused(said!(
            "The list {} already uses the same file name. Give this one another name.",
            other.name
        )));
    }
    let bases = old.map(|k| k.bases).unwrap_or_default();
    write(dir, name, ids, bases)
}

/// A list built on brings in its mods, but for those `name` turned off.
pub fn build_on(dir: &Path, name: &str, bases: &[String]) -> Result<(), Error> {
    let mut ids = load(dir, name)?;
    let drops = read(&file_for(dir, name))
        .map(|k| k.drops)
        .unwrap_or_default();
    let (given, _) = given(dir, name, bases, &mut Vec::new())?;
    merge(&mut ids, &minus(&given, &drops));
    write(dir, name, &ids, bases.to_vec())
}

fn write(dir: &Path, name: &str, ids: &[String], mut bases: Vec<String>) -> Result<(), Error> {
    bases.retain(|base| taken(dir, base));
    let path = file_for(dir, name);
    let mut kept = Kept {
        name: name.trim().to_string(),
        ids: ids.to_vec(),
        ..Kept::default()
    };
    if !bases.is_empty() {
        let (given, _) = given(dir, name, &bases, &mut Vec::new())?;
        kept.adds = minus(ids, &given);
        kept.drops = minus(&given, ids);
        kept.bases = bases;
    }
    crate::base::atomic_file::write_json(&path, &kept)
}

fn minus(from: &[String], gone: &[String]) -> Vec<String> {
    from.iter()
        .filter(|id| !gone.contains(id))
        .cloned()
        .collect()
}

fn read(path: &Path) -> Option<Kept> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn all(dir: &Path) -> Vec<Saved> {
    let mut found: Vec<Saved> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let kept = read(&entry.path())?;
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some(Saved {
                mods: load(dir, &kept.name).map_or(kept.ids.len(), |ids| ids.len()),
                under: under(dir, &kept.name),
                name: kept.name,
                bases: kept.bases,
                adds: kept.adds.len(),
                drops: kept.drops.len(),
                modified: modified.duration_since(UNIX_EPOCH).ok()?.as_secs(),
            })
        })
        .collect();
    found.sort_by_key(|s| s.name.to_lowercase());
    found
}

/// Breadth-first, so the nearest come first; a file may hold a loop, which `found` stops.
fn under(dir: &Path, name: &str) -> Vec<String> {
    let bases = |n: &str| read(&file_for(dir, n)).map(|k| k.bases).unwrap_or_default();
    let mut found: Vec<String> = Vec::new();
    let mut queue = std::collections::VecDeque::from(bases(name));
    while let Some(next) = queue.pop_front() {
        if next != name && !found.contains(&next) {
            queue.extend(bases(&next));
            found.push(next);
        }
    }
    found
}

/// Counts files without opening them: it runs on every change of the load order.
pub fn count(dir: &Path) -> usize {
    let files = std::fs::read_dir(dir).into_iter().flatten().flatten();
    let is_list = |entry: &std::fs::DirEntry| entry.path().extension().is_some_and(|x| x == "json");
    files.filter(is_list).count()
}

pub fn prune(dir: &Path, keep: usize) {
    let mut found = all(dir);
    found.sort_by_key(|s| std::cmp::Reverse(s.modified));
    for old in found.iter().skip(keep) {
        // Best effort: a snapshot that cannot be deleted is only clutter.
        let _ = delete(dir, &old.name);
    }
}

pub fn taken(dir: &Path, name: &str) -> bool {
    file_for(dir, name).exists()
}

pub fn load(dir: &Path, name: &str) -> Result<Vec<String>, Error> {
    resolve(dir, name, &mut Vec::new()).map(|(ids, _)| ids)
}

/// The mods `name` gives from the lists under it, each with the lowest list in the stack that gives it.
pub fn underneath(dir: &Path, name: &str) -> Vec<(String, String)> {
    sources(dir, name)
        .into_iter()
        .filter(|(_, list)| list != name)
        .collect()
}

/// Each mod `name` gives, with the lowest list in its stack that gives it.
fn sources(dir: &Path, name: &str) -> Vec<(String, String)> {
    // A list that loads has no loop below it, so recursing ends.
    let (Ok(ids), Some(kept)) = (load(dir, name), read(&file_for(dir, name))) else {
        return Vec::new();
    };
    let mut below = HashMap::new();
    for (id, list) in kept.bases.iter().flat_map(|base| sources(dir, base)) {
        below.entry(id).or_insert(list);
    }
    ids.into_iter()
        .map(|id| {
            let list = below.remove(&id).unwrap_or_else(|| name.to_string());
            (id, list)
        })
        .collect()
}

/// Its mods in its own order, and the mods turned off in it or under it.
fn resolve(
    dir: &Path,
    name: &str,
    seen: &mut Vec<String>,
) -> Result<(Vec<String>, Vec<String>), Error> {
    if seen.iter().any(|s| s == name) {
        return Err(Error::refused(said!("{} builds on itself", name)));
    }
    let path = file_for(dir, name);
    let kept = read(&path)
        .ok_or_else(|| Error::format(said!("{} is not a saved list", path.display())))?;
    if kept.bases.is_empty() {
        return Ok((kept.ids, Vec::new()));
    }
    // A list it builds on is gone: what it last held, with what the others give.
    if !kept.bases.iter().all(|base| taken(dir, base)) {
        let (given, _) = given(dir, name, &kept.bases, seen)?;
        let mut ids = kept.ids;
        merge(&mut ids, &minus(&given, &kept.drops));
        return Ok((ids, kept.drops));
    }
    let (given, mut dropped) = given(dir, name, &kept.bases, seen)?;
    dropped.extend(kept.drops);
    dropped.retain(|id| !kept.adds.contains(id));
    let mut ids: Vec<String> = kept
        .ids
        .into_iter()
        .filter(|id| kept.adds.contains(id) || (given.contains(id) && !dropped.contains(id)))
        .collect();
    merge(&mut ids, &minus(&given, &dropped));
    Ok((ids, dropped))
}

/// What the lists it builds on give: merged in order, less what any of them turned off.
fn given(
    dir: &Path,
    name: &str,
    bases: &[String],
    seen: &mut Vec<String>,
) -> Result<(Vec<String>, Vec<String>), Error> {
    let (mut merged, mut dropped) = (Vec::new(), Vec::new());
    seen.push(name.to_string());
    for base in bases.iter().filter(|base| taken(dir, base)) {
        let (ids, off) = resolve(dir, base, seen)?;
        merge(&mut merged, &ids);
        dropped.extend(off);
    }
    seen.pop();
    merged.retain(|id| !dropped.contains(id));
    Ok((merged, dropped))
}

/// Each mod `into` lacks goes after the nearest one it follows in `from`, or first.
pub fn merge(into: &mut Vec<String>, from: &[String]) {
    for (i, id) in from.iter().enumerate() {
        if into.contains(id) {
            continue;
        }
        let at = from[..i]
            .iter()
            .rev()
            .find_map(|before| into.iter().position(|x| x == before))
            .map_or(0, |p| p + 1);
        into.insert(at, id.clone());
    }
}

pub fn delete(dir: &Path, name: &str) -> Result<(), Error> {
    let path = file_for(dir, name);
    std::fs::remove_file(&path).map_err(|e| Error::file(&path, e))
}

/// Takes `id` out of `name`, and so out of every list built on it.
pub fn take_out(dir: &Path, name: &str, id: &str) -> Result<(), Error> {
    let key = |id: &str| crate::rimworld::id::split_steam(id).0;
    let mut ids = load(dir, name)?;
    ids.retain(|kept| key(kept) != key(id));
    save(dir, name, &ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_counted_by_their_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(count(tmp.path()), 0);
        save(tmp.path(), "one", &["a".to_string()]).unwrap();
        save(tmp.path(), "two", &[]).unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "").unwrap();
        assert_eq!((count(tmp.path()), all(tmp.path()).len()), (2, 2));
        assert_eq!(count(&tmp.path().join("absent")), 0);
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_list_is_kept_listed_loaded_and_deleted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lists");
        assert!(all(&dir).is_empty());
        save(&dir, "Medieval run", &ids(&["a", "b"])).unwrap();
        save(&dir, "base", &ids(&["a"])).unwrap();
        let found = all(&dir);
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["base", "Medieval run"]);
        assert_eq!(found[1].mods, 2);
        assert_eq!(load(&dir, "Medieval run").unwrap(), ids(&["a", "b"]));
        delete(&dir, "base").unwrap();
        assert_eq!(all(&dir).len(), 1);
        assert!(load(&dir, "base").is_err());
    }

    #[test]
    fn saving_under_the_same_name_replaces_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), "run", &ids(&["a"])).unwrap();
        save(tmp.path(), " run ", &ids(&["b", "c"])).unwrap();
        assert_eq!(all(tmp.path()).len(), 1);
        assert_eq!(load(tmp.path(), "run").unwrap(), ids(&["b", "c"]));
    }

    #[test]
    fn a_name_cannot_leave_the_folder_or_be_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("lists");
        save(&dir, "../../escape/it", &ids(&["a"])).unwrap();
        assert!(!tmp.path().join("escape").exists());
        assert_eq!(all(&dir)[0].name, "../../escape/it");
        assert!(save(&dir, "  ", &ids(&["a"])).is_err());
    }

    #[test]
    fn pruning_keeps_the_given_number_of_lists() {
        let tmp = tempfile::tempdir().unwrap();
        for n in 0..5 {
            save(tmp.path(), &format!("snapshot {n}"), &ids(&["a"])).unwrap();
        }
        prune(tmp.path(), 3);
        assert_eq!(all(tmp.path()).len(), 3);
        prune(tmp.path(), 10);
        assert_eq!(all(tmp.path()).len(), 3);
    }

    #[test]
    fn other_files_in_the_folder_are_not_lists() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("notes.json"), "[1,2]").unwrap();
        std::fs::write(tmp.path().join("x.txt"), "hello").unwrap();
        assert!(all(tmp.path()).is_empty());
    }

    #[test]
    fn saving_a_list_never_replaces_another_whose_name_is_written_alike() {
        let tmp = tempfile::tempdir().unwrap();
        save(tmp.path(), "war_crimes", &ids(&["first.list"])).unwrap();
        let _ = save(tmp.path(), "war/crimes", &ids(&["second.list"]));
        assert_eq!(
            load(tmp.path(), "war_crimes").unwrap(),
            ids(&["first.list"])
        );
    }

    /// As the UI does it: save, build on others, save again.
    fn stacked(dir: &Path, name: &str, bases: &[&str], saved: &[&str]) {
        save(dir, name, &ids(&["core"])).unwrap();
        build_on(dir, name, &ids(bases)).unwrap();
        save(dir, name, &ids(saved)).unwrap();
    }

    fn mods(dir: &Path) {
        save(dir, "base", &ids(&["core", "a", "b", "c"])).unwrap();
        stacked(
            dir,
            "qol",
            &["base"],
            &["core", "a", "b", "c", "d", "e", "f"],
        );
        let medieval = ["core", "a", "b", "c", "d", "g", "h", "i", "f"];
        stacked(dir, "medieval", &["qol"], &medieval);
        stacked(
            dir,
            "modern",
            &["qol"],
            &["core", "a", "b", "c", "d", "e", "f", "z"],
        );
    }

    #[test]
    fn a_list_built_on_others_keeps_what_they_turned_off() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        save(dir, "raiders", &ids(&["core"])).unwrap();
        build_on(dir, "raiders", &ids(&["modern", "medieval"])).unwrap();
        let all = ["core", "a", "b", "c", "d", "g", "h", "i", "f", "z"];
        assert_eq!(load(dir, "raiders").unwrap(), ids(&all));
        let raiders = super::all(dir).into_iter().find(|s| s.name == "raiders");
        let under = ids(&["modern", "medieval", "qol", "base"]);
        assert_eq!(raiders.unwrap().under, under);
        let without_z = &all[..9];
        save(dir, "raiders", &ids(without_z)).unwrap();
        assert_eq!(load(dir, "raiders").unwrap(), ids(without_z));
    }

    #[test]
    fn a_mod_taken_out_of_a_list_is_gone_from_the_lists_built_on_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        take_out(dir, "base", "B_steam").unwrap();
        let qol = ["core", "a", "c", "d", "e", "f"];
        assert_eq!(load(dir, "qol").unwrap(), ids(&qol));
        assert!(!load(dir, "medieval").unwrap().contains(&"b".to_string()));
    }

    #[test]
    fn a_list_follows_the_lists_it_builds_on() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        save(dir, "base", &ids(&["core", "a", "x", "c"])).unwrap();
        let qol = ["core", "a", "x", "c", "d", "e", "f"];
        assert_eq!(load(dir, "qol").unwrap(), ids(&qol));
        let medieval = ["core", "a", "x", "c", "d", "g", "h", "i", "f"];
        assert_eq!(load(dir, "medieval").unwrap(), ids(&medieval));
        assert!(underneath(dir, "base").is_empty());
        let from = |list: &str, mods: &[&str]| -> Vec<(String, String)> {
            mods.iter()
                .map(|m| (m.to_string(), list.to_string()))
                .collect()
        };
        let mut under = from("base", &["core", "a", "x", "c"]);
        under.extend(from("qol", &["d", "f"]));
        assert_eq!(underneath(dir, "medieval"), under);
        save(dir, "base", &ids(&["core"])).unwrap();
        let medieval = all(dir).into_iter().find(|s| s.name == "medieval").unwrap();
        assert_eq!((medieval.mods, medieval.adds, medieval.drops), (6, 3, 1));
    }

    #[test]
    fn a_mod_turned_off_below_comes_back_when_saved_on() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        let mine = ["core", "a", "b", "c", "d", "e", "g", "h", "i", "f"];
        stacked(dir, "mine", &["medieval"], &mine);
        assert_eq!(load(dir, "mine").unwrap(), ids(&mine));
    }

    #[test]
    fn only_a_list_built_on_others_is_written_with_what_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        save(dir, "base", &ids(&["a"])).unwrap();
        let plain = std::fs::read_to_string(dir.join("base.json")).unwrap();
        assert_eq!(plain, r#"{"name":"base","ids":["a"]}"#);
        stacked(dir, "top", &["base"], &["a", "b"]);
        save(dir, "top", &ids(&["b"])).unwrap();
        let top = all(dir).into_iter().find(|s| s.name == "top").unwrap();
        assert_eq!(top.bases, ids(&["base"]));
        let text = std::fs::read_to_string(dir.join("top.json")).unwrap();
        assert!(text.contains(r#""adds":["b"],"drops":["a"]"#));
    }

    #[test]
    fn a_list_cannot_build_on_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        assert!(build_on(dir, "base", &ids(&["qol"])).is_err());
        assert_eq!(load(dir, "base").unwrap(), ids(&["core", "a", "b", "c"]));
        assert!(build_on(dir, "base", &ids(&["base"])).is_err());
    }

    #[test]
    fn a_list_whose_base_is_gone_is_as_last_saved_until_saved_again() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        mods(dir);
        let modern = load(dir, "modern").unwrap();
        std::fs::remove_file(file_for(dir, "qol")).unwrap();
        assert_eq!(load(dir, "modern").unwrap(), modern);
        build_on(dir, "modern", &ids(&["qol", "base"])).unwrap();
        assert_eq!(load(dir, "modern").unwrap(), modern);
        let saved = super::all(dir).into_iter().find(|s| s.name == "modern");
        assert_eq!(saved.unwrap().bases, ids(&["base"]));
    }
}
