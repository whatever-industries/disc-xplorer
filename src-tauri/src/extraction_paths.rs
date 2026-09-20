//! Check destination names before extraction writes any members of a directory.
//! Sanitization is deliberately portable, so collision checks are case-insensitive
//! on every host too. A source image must never silently replace its own files.

use std::collections::HashMap;

fn collision(first: &str, second: &str) -> String {
    format!("Cannot extract: {first:?} and {second:?} have conflicting destination names. Extract them separately with different names.")
}

pub(crate) fn validate_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashMap::new();
    for name in names {
        let key = crate::sanitize_component(name).to_lowercase();
        if let Some(first) = seen.insert(key, name) {
            return Err(collision(first, name));
        }
    }
    Ok(())
}

/// Flat archives need to check implicit parent directories as well as files:
/// `a?/one` and `a*/two` must not silently merge into the same `a_` folder.
pub(crate) fn validate_paths<'a>(paths: impl IntoIterator<Item = (&'a str, bool)>) -> Result<(), String> {
    let mut seen: HashMap<String, (String, bool)> = HashMap::new();
    for (path, is_dir) in paths {
        let parts: Vec<_> = path.split('/').collect();
        let mut source = String::new();
        let mut destination = String::new();
        for (i, name) in parts.iter().enumerate() {
            if i > 0 {
                source.push('/');
                destination.push('/');
            }
            source.push_str(name);
            destination.push_str(&crate::sanitize_component(name).to_lowercase());
            let directory = i + 1 < parts.len() || is_dir;
            if let Some((first, first_dir)) = seen.get(&destination) {
                // Several members may legitimately share the same parent.
                if first != &source || !directory || !first_dir {
                    return Err(collision(first, &source));
                }
            } else {
                seen.insert(destination.clone(), (source.clone(), directory));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sanitized_and_case_collisions() {
        for names in [["track?.txt", "track_.txt"], ["Readme", "README"], ["CON", "_CON"], ["name.", "name"]] {
            let error = validate_names(names).unwrap_err();
            assert!(error.contains(names[0]) && error.contains(names[1]));
        }
        validate_names(["one.txt", "two.txt"]).unwrap();
    }

    #[test]
    fn rejects_directory_merges_and_file_directory_conflicts() {
        for entries in [
            [("a?/one", false), ("a*/two", false)],
            [("a?", false), ("a_/child", false)],
            [("a_/child", false), ("a?", false)],
            [("a?", true), ("a_", true)],
            [("dir/Readme", false), ("dir/README", false)],
        ] {
            assert!(validate_paths(entries).is_err(), "{entries:?}");
        }
    }

    #[test]
    fn allows_shared_parents_and_identical_names_in_separate_directories() {
        validate_paths([("a", true), ("a/one", false), ("a/two", false), ("b/one", false)]).unwrap();
    }
}
