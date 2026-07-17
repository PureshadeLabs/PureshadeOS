//! Realize-guard tests (host). Cover the stage-2 requirements: temp writable
//! before seal, realize (temp → atomic rename) seals, write to a realized path
//! is rejected, re-realize is a no-op, a sealed object cannot be moved, and two
//! concurrent realizes of the same digest converge to one sealed object with no
//! error and no corruption.

use super::*;
use crate::backend::{FsBackend, FsError};
use crate::testutil::MemBackend;

const NAME: &str = "abcd1234-demo-1.0";

#[test]
fn temp_writable_before_seal() {
    let g = RealizeGuard::new();
    // Nothing sealed → temp and any path is writable.
    assert!(g.check_mutate("/.tmp-a").is_ok());
    assert!(g.check_mutate("/.tmp-a/bin/x").is_ok());
    assert!(g.check_mutate(&alloc::format!("/{NAME}/bin/x")).is_ok());
}

#[test]
fn write_to_sealed_rejected() {
    let mut g = RealizeGuard::new();
    g.seal(NAME);
    assert_eq!(g.check_mutate(&alloc::format!("/{NAME}")), Err(FsError::ReadOnly));
    assert_eq!(g.check_mutate(&alloc::format!("/{NAME}/bin/x")), Err(FsError::ReadOnly));
    // A different, unsealed name stays writable.
    assert!(g.check_mutate("/other-2.0/bin/x").is_ok());
}

#[test]
fn rename_seals_and_blocks() {
    let mut g = RealizeGuard::new();
    let dst = alloc::format!("/{NAME}");
    // First realize: temp → final name proceeds, then seal.
    match g.check_rename("/.tmp-a", &dst).unwrap() {
        RenameOutcome::Proceed { store_name } => {
            assert_eq!(store_name, NAME);
            g.seal(&store_name);
        }
        other => panic!("expected Proceed, got {other:?}"),
    }
    // Now the realized path is immutable.
    assert_eq!(g.check_mutate(&dst), Err(FsError::ReadOnly));
    assert_eq!(g.check_mutate(&alloc::format!("/{NAME}/bin/x")), Err(FsError::ReadOnly));
}

#[test]
fn re_realize_is_noop() {
    let mut g = RealizeGuard::new();
    g.seal(NAME);
    // A second realize renames onto the already-sealed name → no-op, no error.
    assert_eq!(g.check_rename("/.tmp-b", &alloc::format!("/{NAME}")).unwrap(), RenameOutcome::NoOp);
    assert_eq!(g.sealed_count(), 1);
}

#[test]
fn sealed_source_cannot_move() {
    let mut g = RealizeGuard::new();
    g.seal(NAME);
    // Moving a sealed object away is a mutation of immutable content.
    assert_eq!(
        g.check_rename(&alloc::format!("/{NAME}"), "/elsewhere"),
        Err(FsError::ReadOnly)
    );
}

#[test]
fn nested_into_sealed_rejected() {
    let mut g = RealizeGuard::new();
    g.seal(NAME);
    // Renaming a temp *inside* a sealed object is rejected (not a no-op).
    assert_eq!(
        g.check_rename("/.tmp-c", &alloc::format!("/{NAME}/inject")),
        Err(FsError::ReadOnly)
    );
}

#[test]
fn forget_after_whole_path_removal() {
    let mut g = RealizeGuard::new();
    g.seal(NAME);
    assert_eq!(g.check_mutate(&alloc::format!("/{NAME}/bin/x")), Err(FsError::ReadOnly));
    // While sealed there is NO in-place exit — the seal is absolute. `forget`
    // is only called by the kernel once the whole tree is deleted below the
    // seal (store_remove_tree); it drops the now-nonexistent name from the set.
    // Idempotent: a second forget reports it was already gone.
    assert!(g.forget(NAME), "forget must report the name was sealed");
    assert!(!g.forget(NAME), "second forget is a no-op");
    assert_eq!(g.sealed_count(), 0);
}

/// Drive one realize against a `MemBackend` store + guard, exactly as the
/// kernel glue will: stage a temp file with `content`, then consult the guard
/// on the rename. Returns the outcome.
fn realize(
    be: &mut MemBackend,
    g: &mut RealizeGuard,
    temp: &str,
    final_name: &str,
    content: &[u8],
) -> RenameOutcome {
    // Stage: temp is unsealed, so writing it is allowed.
    let dst = alloc::format!("/{final_name}");
    assert!(g.check_mutate(temp).is_ok());
    let ino = be.create(temp).unwrap();
    be.write_at(ino, 0, content).unwrap();
    be.commit().unwrap();

    match g.check_rename(temp, &dst).unwrap() {
        RenameOutcome::Proceed { store_name } => {
            be.rename(temp, &dst).unwrap();
            be.commit().unwrap();
            g.seal(&store_name);
            RenameOutcome::Proceed { store_name }
        }
        RenameOutcome::NoOp => {
            // Loser: drop the redundant temp, leave the winner's object intact.
            be.unlink(temp).unwrap();
            be.commit().unwrap();
            RenameOutcome::NoOp
        }
    }
}

#[test]
fn concurrent_same_digest_converges() {
    let mut be = MemBackend::new();
    let mut g = RealizeGuard::new();
    let content = b"\x7fELF identical bytes";

    // Two writers, distinct temps, identical content, same final digest name.
    let o1 = realize(&mut be, &mut g, "/.tmp-writerA", NAME, content);
    let o2 = realize(&mut be, &mut g, "/.tmp-writerB", NAME, content);

    // First proceeds and seals; second is the idempotent no-op.
    assert!(matches!(o1, RenameOutcome::Proceed { .. }));
    assert_eq!(o2, RenameOutcome::NoOp);

    // Converged: exactly one sealed object, no temps left, correct content.
    assert_eq!(g.sealed_count(), 1);
    assert!(g.is_sealed_name(NAME));
    let mut names: alloc::vec::Vec<_> =
        be.readdir("/").unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    assert_eq!(names, alloc::vec![NAME.to_string()], "one object, no leftover temps");

    let ino = be.lookup(&alloc::format!("/{NAME}")).unwrap();
    let mut buf = alloc::vec![0u8; content.len()];
    let n = be.read_at(ino, 0, &mut buf).unwrap();
    assert_eq!(&buf[..n], content, "winner's content intact, uncorrupted");

    // And the realized object is now immutable at the mount boundary.
    assert_eq!(g.check_mutate(&alloc::format!("/{NAME}/bin")), Err(FsError::ReadOnly));
}
