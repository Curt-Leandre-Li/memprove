//! U1 U-F (SF-2): `rebind_restored_state_paths` binds a restored registry to
//! its new data directory: only `state_db_path`, only of the rows that named
//! their state database below the backup's source, and nothing else.

use memprove_store_sqlite::{RegistryStore, rebind_restored_state_paths};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const BOUND: &str = "01a13995-0000-7000-8000-000000000001";
const OTHER: &str = "01a13995-0000-7000-8000-000000000002";

fn rows(registry: &std::path::Path) -> Vec<Vec<String>> {
    let connection = Connection::open(registry).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT project_id, repository_common_dir, object_format, state_db_path,
                    provisioning_state, created_at, updated_at, CAST(revision AS TEXT)
             FROM projects ORDER BY project_id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            (0..8)
                .map(|index| row.get::<_, String>(index))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn only_the_state_paths_below_the_source_are_rebound() {
    let container = TempDir::new().unwrap();
    let base = container.path().canonicalize().unwrap();
    let source = base.join("old");
    let data = base.join("new");
    drop(RegistryStore::open(&data).unwrap());
    let registry = data.join("registry.sqlite");
    {
        let connection = Connection::open(&registry).unwrap();
        for (project, path) in [
            (BOUND, source.join(format!("projects/{BOUND}/state.sqlite"))),
            (OTHER, base.join(format!("elsewhere/{OTHER}/state.sqlite"))),
        ] {
            connection
                .execute(
                    "INSERT INTO projects VALUES (?1, ?2, 'sha1', ?3, 'ACTIVE',
                     '2026-10-14T00:00:00Z', '2026-10-14T00:00:01Z', 3)",
                    params![
                        project,
                        format!("/repo/{project}/.git"),
                        path.display().to_string()
                    ],
                )
                .unwrap();
        }
    }
    let before = rows(&registry);

    assert_eq!(rebind_restored_state_paths(&data, &source).unwrap(), 1);
    let after = rows(&registry);
    let mut expected = before.clone();
    expected[0][3] = data
        .join(format!("projects/{BOUND}/state.sqlite"))
        .display()
        .to_string();
    assert_eq!(after, expected);

    // A second run, or one from the same directory, writes nothing.
    assert_eq!(rebind_restored_state_paths(&data, &source).unwrap(), 0);
    assert_eq!(rebind_restored_state_paths(&data, &data).unwrap(), 0);
    assert_eq!(rows(&registry), expected);
}
