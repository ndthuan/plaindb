use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

struct Row {
    anchor: &'static str,
    tests: &'static [&'static str],
}

const TABLE: &[Row] = &[
    Row {
        anchor: "atomic-tx",
        tests: &[
            "writes::a_failing_op_leaves_every_file_unchanged",
            "crash::a_crash_after_any_number_of_files_rolls_forward_on_open",
        ],
    },
    Row {
        anchor: "crash",
        tests: &[
            "crash::a_crash_before_the_journal_leaves_the_store_untouched",
            "crash::a_crash_after_any_number_of_files_rolls_forward_on_open",
            "crash::a_crash_at_any_checkpoint_step_loses_nothing",
            "git_commit::a_crash_after_the_commit_is_not_committed_twice",
            "soak::writers_killed_at_random_lose_no_commit_and_no_hand_edit",
        ],
    },
    Row {
        anchor: "multi-process",
        tests: &[
            "multiprocess::four_processes_inserting_at_once_lose_nothing",
            "multiprocess::two_processes_patching_one_document_never_both_win",
        ],
    },
    Row {
        anchor: "isolation",
        tests: &[
            "multiprocess::every_read_during_a_transaction_sees_all_of_it_or_none",
            "multiprocess::a_store_whose_lock_cannot_be_opened_is_read_through_a_checked_scan",
        ],
    },
    Row {
        anchor: "hand-edit",
        tests: &[
            "writes::a_hand_edit_after_open_is_never_overwritten",
            "writes::a_file_created_by_hand_after_open_is_not_overwritten",
            "crash::a_hand_edit_after_the_commit_survives_the_replay",
            "crash::a_file_removed_by_hand_after_the_commit_stays_removed",
        ],
    },
    Row {
        anchor: "round-trip",
        tests: &[
            "codec::every_format_round_trips",
            "codec::markdown_body_survives_byte_for_byte",
            "transfer::a_markdown_body_survives_import_and_export_byte_for_byte",
            "git_restore::a_clone_of_the_remote_opens_as_the_same_store",
        ],
    },
    Row {
        anchor: "time-travel",
        tests: &["git_history::a_document_is_read_at_each_commit_and_time"],
    },
    Row {
        anchor: "no-force-push",
        tests: &[
            "git_push::a_remote_that_is_ahead_is_never_overwritten",
            "git_push::a_remote_with_unrelated_history_is_refused",
        ],
    },
];

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn guarantees_doc() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guarantees.md"))
        .unwrap()
}

fn anchors_in(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!-- g:") {
        let after = &rest[start + "<!-- g:".len()..];
        let Some(end) = after.find(" -->") else {
            break;
        };
        found.insert(after[..end].to_owned());
        rest = &after[end..];
    }
    found
}

fn defines(tests_dir: &Path, test: &str) -> bool {
    let Some((file, name)) = test.split_once("::") else {
        return false;
    };
    let Ok(source) = fs::read_to_string(tests_dir.join(format!("{file}.rs"))) else {
        return false;
    };
    let declaration = format!("fn {name}(");
    source
        .lines()
        .any(|line| line.trim_start().starts_with(&declaration))
}

fn problems(text: &str, table: &[Row], tests_dir: &Path) -> Vec<String> {
    let anchors = anchors_in(text);
    let mut found = Vec::new();
    for anchor in &anchors {
        match table.iter().find(|row| row.anchor == anchor) {
            None => found.push(format!("the guarantee {anchor} has no row")),
            Some(row) if row.tests.is_empty() => {
                found.push(format!("the guarantee {anchor} names no test"));
            }
            Some(_) => {}
        }
    }
    for row in table {
        if !anchors.contains(row.anchor) {
            found.push(format!(
                "the row {} has no guarantee in docs/guarantees.md",
                row.anchor
            ));
        }
        for test in row.tests {
            if !defines(tests_dir, test) {
                found.push(format!(
                    "the row {} names a missing test {test}",
                    row.anchor
                ));
            }
        }
    }
    found
}

#[test]
fn every_guarantee_in_the_docs_maps_to_existing_tests() {
    let found = problems(&guarantees_doc(), TABLE, &tests_dir());
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_guarantee_without_a_row_is_reported() {
    let text = "- It holds. <!-- g:atomic-tx -->\n- It also holds. <!-- g:unmapped -->\n";
    let found = problems(text, &TABLE[..1], &tests_dir());
    assert_eq!(found, ["the guarantee unmapped has no row"]);
}

#[test]
fn a_row_without_a_guarantee_is_reported() {
    let found = problems(
        "- It holds. <!-- g:atomic-tx -->\n",
        &TABLE[..2],
        &tests_dir(),
    );
    assert_eq!(
        found,
        ["the row crash has no guarantee in docs/guarantees.md"]
    );
}

#[test]
fn a_row_naming_a_missing_test_is_reported() {
    const ROWS: &[Row] = &[Row {
        anchor: "atomic-tx",
        tests: &["writes::no_such_test", "no_such_file::anything", "writes"],
    }];
    let found = problems("<!-- g:atomic-tx -->\n", ROWS, &tests_dir());
    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn a_row_with_no_test_is_reported() {
    const ROWS: &[Row] = &[Row {
        anchor: "atomic-tx",
        tests: &[],
    }];
    let found = problems("<!-- g:atomic-tx -->\n", ROWS, &tests_dir());
    assert_eq!(found, ["the guarantee atomic-tx names no test"]);
}
