//! End-to-end migration tests: legacy rows in native PostgreSQL → `run` → the mock Canvas
//! Cloud (which really applies the Yjs updates and computes §6.3 digests).

#![allow(clippy::too_many_lines, clippy::many_single_char_names)] // long, linear scenarios

mod common;

use common::mock::APP_KEY;
use common::{opts, Canvas, Client, Env};
use copper_cloud_canvas_migrate::{MigrateOptions, Status};
use copper_cloud_core::ids;
use serde_json::Value;
use uuid::Uuid;
use yrs::types::text::Text as _;
use yrs::updates::decoder::Decode as _;
use yrs::{any, Any, Array as _, Doc, Map as _, MapPrelim, Out, Transact as _, Update};

struct World {
    alice: Uuid,
    bob: Uuid,
    carol: Uuid,
    dave: Uuid,
    /// Shared: snapshot + tail, members, invites, a share link, mentions.
    roadmap: Canvas,
    /// Alice's Personal canvas.
    personal: Canvas,
    /// Shared canvas with no document rows at all.
    empty: Canvas,
    /// Shared canvas with more updates than one read page.
    busy: Canvas,
    link_id: Uuid,
    link_token: String,
    invites: Vec<(Uuid, String, &'static str)>,
    mention_ab: Uuid,
}

fn apply(client: &Client, updates: &[&[u8]]) {
    let mut txn = client.doc.transact_mut();
    for u in updates {
        txn.apply_update(Update::decode_v1(u).unwrap()).unwrap();
    }
}

async fn world(env: &Env) -> World {
    let l = &env.legacy;
    let alice = l.user("alice@example.com", "Alice", false).await;
    let bob = l.user("bob@example.com", "Bob", false).await;
    let carol = l.user("carol@example.com", "", false).await;
    let dave = l.user("dave@example.com", "Dave", true).await;

    // Roadmap: two clients editing concurrently, then a compaction, then a tail.
    let mut roadmap = l.canvas(alice, "Roadmap", "shared").await;
    l.member(&roadmap, bob).await;
    l.member(&roadmap, carol).await;
    let a = Client::new(101, &roadmap.log);
    let b = Client::new(202, &roadmap.log);
    let u1 = a.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        shapes.insert(
            &mut t,
            "s1",
            MapPrelim::from([
                ("type", Any::from("sticky")),
                ("x", Any::from(10)),
                ("y", Any::from(20.5)),
                ("text", Any::from("Ship it")),
            ]),
        );
        shapes.insert(
            &mut t,
            "s2",
            MapPrelim::from([("type", Any::from("frame")), ("w", Any::from(480.0))]),
        );
    });
    l.append(&mut roadmap, &u1).await;
    let u2 = b.edit(|d| {
        let chat = d.get_or_insert_array("chat");
        let agents = d.get_or_insert_map("agents");
        let mut t = d.transact_mut();
        chat.push_back(
            &mut t,
            any!({"id": "m1", "authorId": bob.to_string(), "text": "@Alice timeline?", "at": 1_760_000_000_000_i64}),
        );
        agents.insert(&mut t, "agent-1", any!({"name": "Scout", "cursor": null, "status": "idle"}));
    });
    l.append(&mut roadmap, &u2).await;
    apply(&b, &[&u1]);
    let u3 = b.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let notes = d.get_or_insert_text("notes");
        let mut t = d.transact_mut();
        if let Some(Out::YMap(s1)) = shapes.get(&t, "s1") {
            s1.insert(&mut t, "x", 42.25);
        }
        notes.insert(&mut t, 0, "Q4 — draft ✓");
    });
    let seq3 = l.append(&mut roadmap, &u3).await;
    l.compact(&roadmap, seq3).await;
    apply(&a, &[&u2, &u3]);
    let u4 = a.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let meta = d.get_or_insert_map("meta");
        let mut t = d.transact_mut();
        shapes.remove(&mut t, "s2");
        meta.insert(&mut t, "thumb", Any::Buffer(vec![0_u8, 1, 2, 250].into()));
    });
    l.append(&mut roadmap, &u4).await;
    let u5 = b.edit(|d| {
        let chat = d.get_or_insert_array("chat");
        let mut t = d.transact_mut();
        chat.push_back(
            &mut t,
            any!({"id": "m2", "authorId": bob.to_string(), "text": "🚀 \"quoted\" \\ done"}),
        );
    });
    l.append(&mut roadmap, &u5).await;

    let invites = vec![
        (
            l.invite(&roadmap, "erin@example.com", Some(alice), "pending", true)
                .await,
            "pending",
        ),
        (
            l.invite(&roadmap, "bob@example.com", Some(alice), "accepted", false)
                .await,
            "accepted",
        ),
        (
            l.invite(&roadmap, "frank@example.com", Some(bob), "revoked", false)
                .await,
            "revoked",
        ),
        (
            l.invite(&roadmap, "gina@example.com", None, "declined", false)
                .await,
            "declined",
        ),
    ]
    .into_iter()
    .map(|((id, token), status)| (id, token, status))
    .collect();
    let (link_id, link_token) = l.share_link(&roadmap, alice, 3).await;
    let mention_ab = l
        .mention(
            &roadmap,
            alice,
            bob,
            "m1-reply",
            "@Bob see the timeline ✓",
            false,
        )
        .await;
    l.mention(&roadmap, bob, carol, "m2", "@carol done", true)
        .await;

    let mut personal = l.canvas(alice, "Personal", "personal").await;
    let p = Client::new(303, &personal.log);
    let pu = p.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        shapes.insert(
            &mut t,
            "p1",
            MapPrelim::from([("type", Any::from("text")), ("text", Any::from("mine"))]),
        );
    });
    l.append(&mut personal, &pu).await;

    let empty = l.bare_canvas(bob, "Empty", "shared").await;

    let mut busy = l.canvas(carol, "Busy", "shared").await;
    let c = Client::new(404, &busy.log);
    for i in 0..40 {
        let u = c.edit(|d| {
            let shapes = d.get_or_insert_map("shapes");
            let mut t = d.transact_mut();
            shapes.insert(&mut t, format!("b{}", i % 7), any!({"n": i}));
        });
        l.append(&mut busy, &u).await;
    }

    World {
        alice,
        bob,
        carol,
        dave,
        roadmap,
        personal,
        empty,
        busy,
        link_id,
        link_token,
        invites,
        mention_ab,
    }
}

fn row(
    report: &copper_cloud_canvas_migrate::MigrateReport,
    id: Uuid,
) -> &copper_cloud_canvas_migrate::CanvasReport {
    report
        .canvases
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no row for {id}"))
}

#[tokio::test]
async fn full_run_migrates_everything_verified() {
    let env = Env::new().await;
    let w = world(&env).await;
    let before = env.legacy_fingerprint().await;

    let report = env.migrate(opts()).await.unwrap();
    assert!(report.ok(), "{report}");
    assert_eq!(report.users.provisioned, 4);
    assert_eq!(report.users.created, 4);
    assert_eq!(report.totals.migrated, 4);
    assert_eq!(report.totals.failed, 0);
    // Legacy order (created_at).
    let order: Vec<Uuid> = report.canvases.iter().map(|c| c.id).collect();
    assert_eq!(
        order,
        vec![w.roadmap.id, w.personal.id, w.empty.id, w.busy.id]
    );

    // Users: same ids, display names and disabled flags passed through.
    env.mock.with(|s| {
        for (u, email) in [
            (w.alice, "alice"),
            (w.bob, "bob"),
            (w.carol, "carol"),
            (w.dave, "dave"),
        ] {
            assert_eq!(s.users.get(&u.to_string()), Some(&u), "{email}");
        }
        let carol = s
            .user_payloads
            .iter()
            .find(|p| p["subject"] == w.carol.to_string())
            .unwrap();
        assert!(carol["display_name"].is_null(), "empty names go as null");
        assert_eq!(carol["id"], w.carol.to_string());
        let dave = s
            .user_payloads
            .iter()
            .find(|p| p["subject"] == w.dave.to_string())
            .unwrap();
        assert_eq!(dave["disabled"], true);
    });

    // Every canvas: digest = the ground truth of its full update log (snapshot + tail).
    for c in [&w.roadmap, &w.personal, &w.busy] {
        let r = row(&report, c.id);
        assert_eq!(r.status, Status::Migrated, "{r:?}");
        assert_eq!(r.digest.as_deref(), Some(c.truth().as_str()), "{}", c.id);
        assert_eq!(
            env.mock.stored_digest(c.id).as_deref(),
            Some(c.truth().as_str())
        );
    }

    // Roadmap details.
    let r = row(&report, w.roadmap.id);
    assert!(r.snapshot);
    assert_eq!(r.updates, 3, "snapshot + 2 tail updates");
    assert_eq!((r.members, r.invites, r.links, r.mentions), (3, 4, 1, 2));
    let got = env.mock.received(w.roadmap.id).unwrap();
    assert_eq!(got.canvas["name"], "Roadmap");
    assert_eq!(got.canvas["kind"], "shared");
    assert_eq!(got.canvas["owner"], w.alice.to_string());
    assert_eq!(got.members[0]["role"], "owner");
    assert_eq!(got.members[0]["subject"], w.alice.to_string());
    let mut member_ids: Vec<&str> = got
        .members
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    member_ids.sort_unstable();
    let mut expected = vec![w.alice.to_string(), w.bob.to_string(), w.carol.to_string()];
    expected.sort();
    assert_eq!(member_ids, expected);
    // Invites: every status, the raw token digest, nudged_at.
    for (id, token, status) in &w.invites {
        let inv = got
            .invites
            .iter()
            .find(|i| i["id"] == id.to_string())
            .unwrap();
        assert_eq!(inv["status"], *status);
        assert_eq!(inv["token_sha256_hex"], token.as_str());
        assert_eq!(inv["nudged_at"].is_string(), *status == "pending");
        assert_eq!(inv["accepted_at"].is_string(), *status == "accepted");
    }
    let declined = got
        .invites
        .iter()
        .find(|i| i["status"] == "declined")
        .unwrap();
    assert_eq!(declined["invited_by"], Value::Null);
    // Share link: the digest of the existing token is preserved, so the token keeps working.
    let link = &got.links[0];
    assert_eq!(link["id"], w.link_id.to_string());
    assert_eq!(
        link["token_sha256_hex"],
        ids::sha256_hex(w.link_token.as_bytes())
    );
    assert_eq!(link["uses"], 3);
    assert_eq!(link["created_by"], w.alice.to_string());
    // Mention: the excerpt arrives as plaintext.
    let m = got
        .mentions
        .iter()
        .find(|m| m["id"] == w.mention_ab.to_string())
        .unwrap();
    assert_eq!(m["excerpt"], "@Bob see the timeline ✓");
    assert_eq!(m["from"], w.alice.to_string());
    assert_eq!(m["to"], w.bob.to_string());
    assert_eq!(m["message_id"], "m1-reply");
    assert_eq!(m["read_at"], Value::Null);
    assert!(got
        .mentions
        .iter()
        .any(|m| m["excerpt"] == "@carol done" && m["read_at"].is_string()));

    // Zero-update canvas.
    let e = row(&report, w.empty.id);
    assert_eq!(e.status, Status::Migrated);
    assert_eq!((e.updates, e.bytes, e.snapshot), (0, 0, false));
    assert_eq!(env.mock.received(w.empty.id).unwrap().updates, 0);
    // Paged reads: more updates than one page.
    assert_eq!(row(&report, w.busy.id).updates, 41);

    // Ledger: all migrated with the verified digest and update counts.
    let ledger = env.ledger().await;
    assert_eq!(ledger.len(), 4);
    for (id, status, digest, updates) in &ledger {
        let r = row(&report, *id);
        assert_eq!(status, "migrated");
        assert_eq!(digest.as_deref(), r.digest.as_deref());
        assert_eq!(*updates, Some(i32::try_from(r.updates).unwrap()));
    }
    let (_, _, detail) = env.ledger_row(w.roadmap.id).await.unwrap();
    assert_eq!(detail["import_status"], "created");
    assert_eq!(detail["mentions"], 2);

    // Legacy rows untouched; the table and JSON renderings work.
    assert_eq!(env.legacy_fingerprint().await, before);
    let table = report.table();
    assert!(
        table.contains("Roadmap") && table.contains("migrated") && table.trim_end().ends_with("ok"),
        "{table}"
    );
    let json: Value = serde_json::to_value(&report).unwrap();
    assert_eq!(json["totals"]["migrated"], 4);
    assert_eq!(json["ok"], true);
    env.finish().await;
}

#[tokio::test]
async fn reruns_are_idempotent_and_never_trust_unverified_canvases() {
    let env = Env::new().await;
    let w = world(&env).await;
    assert!(env.migrate(opts()).await.unwrap().ok());
    let posts = env.mock.with(|s| s.import_posts);

    // Plain re-run: the ledger says migrated → nothing is sent.
    let again = env.migrate(opts()).await.unwrap();
    assert!(again.ok(), "{again}");
    assert_eq!(again.totals.already, 4);
    assert!(again.canvases.iter().all(|c| c.digest.is_some()));
    assert_eq!(env.mock.with(|s| s.import_posts), posts);

    // --only on a migrated canvas: canvas-cloud's digest matches the ledger → already.
    let only = env
        .migrate(MigrateOptions {
            only: vec![w.roadmap.id],
            ..opts()
        })
        .await
        .unwrap();
    assert!(only.ok(), "{only}");
    assert_eq!(only.canvases.len(), 1);
    assert_eq!(only.canvases[0].status, Status::Already);
    assert_eq!(
        env.mock.with(|s| s.import_posts),
        posts,
        "pre-check avoids the upload"
    );

    // Crash between import and verification: ledger `imported` + matching digest → confirmed.
    sqlx::query("UPDATE canvas_migrations SET status = 'imported' WHERE canvas_id = $1")
        .bind(w.personal.id)
        .execute(&env.db.pool)
        .await
        .unwrap();
    let confirm = env.migrate(opts()).await.unwrap();
    assert!(confirm.ok(), "{confirm}");
    assert_eq!(row(&confirm, w.personal.id).status, Status::Already);
    assert_eq!(env.ledger_row(w.personal.id).await.unwrap().0, "migrated");

    // No ledger record but the canvas exists in canvas-cloud: not provably the same → failed,
    // and the hint says how to fix it.
    sqlx::query("DELETE FROM canvas_migrations WHERE canvas_id = $1")
        .bind(w.busy.id)
        .execute(&env.db.pool)
        .await
        .unwrap();
    let unknown = env.migrate(opts()).await.unwrap();
    assert!(!unknown.ok());
    let r = row(&unknown, w.busy.id);
    assert_eq!(r.status, Status::Failed);
    assert!(r.error.as_deref().unwrap().contains("--replace"), "{r:?}");
    assert_eq!(env.ledger_row(w.busy.id).await.unwrap().0, "failed");
    assert_eq!(
        env.mock.with(|s| s.import_posts),
        posts,
        "still nothing uploaded"
    );

    // --replace --only fixes it.
    let fixed = env
        .migrate(MigrateOptions {
            only: vec![w.busy.id],
            replace: true,
            ..opts()
        })
        .await
        .unwrap();
    assert!(fixed.ok(), "{fixed}");
    assert_eq!(fixed.canvases[0].status, Status::Replaced);
    assert_eq!(env.ledger_row(w.busy.id).await.unwrap().0, "migrated");
    env.finish().await;
}

#[tokio::test]
async fn only_limits_the_run_and_reports_unknown_ids() {
    let env = Env::new().await;
    let w = world(&env).await;
    let stranger = Uuid::now_v7();
    let report = env
        .migrate(MigrateOptions {
            only: vec![w.empty.id, stranger, w.personal.id],
            ..opts()
        })
        .await
        .unwrap();
    assert!(!report.ok(), "an unknown --only id fails the run");
    let ids: Vec<Uuid> = report.canvases.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![w.personal.id, w.empty.id, stranger]);
    assert_eq!(row(&report, w.personal.id).status, Status::Migrated);
    assert_eq!(row(&report, w.empty.id).status, Status::Migrated);
    assert!(row(&report, stranger)
        .error
        .as_deref()
        .unwrap()
        .contains("not a legacy canvas"));
    assert!(env.mock.received(w.roadmap.id).is_none());
    assert_eq!(env.ledger().await.len(), 2);
    // Users are provisioned in full regardless of --only.
    assert_eq!(report.users.provisioned, 4);
    env.finish().await;
}

#[tokio::test]
async fn replace_reimports_canvases_that_changed_in_copper() {
    let env = Env::new().await;
    let mut w = world(&env).await;
    assert!(env.migrate(opts()).await.unwrap().ok());
    let old = w.roadmap.truth();

    // A rollback period: the legacy canvas keeps changing in the old copper-cloud.
    let c = Client::new(909, &w.roadmap.log);
    let u = c.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        shapes.insert(
            &mut t,
            "late",
            any!({"type": "sticky", "text": "after rollback"}),
        );
    });
    env.legacy.append(&mut w.roadmap, &u).await;
    let new = w.roadmap.truth();
    assert_ne!(old, new);

    // A plain re-run notices the legacy change and says how to bring it over, changing
    // nothing (and the other canvases stay `already`) …
    let plain = env.migrate(opts()).await.unwrap();
    assert!(!plain.ok(), "{plain}");
    let r = row(&plain, w.roadmap.id);
    assert_eq!(r.status, Status::Failed);
    assert!(
        r.error
            .as_deref()
            .unwrap()
            .contains("changed in the legacy tables"),
        "{r:?}"
    );
    assert_eq!(plain.totals.already, 3, "{plain}");
    assert_eq!(
        env.mock.stored_digest(w.roadmap.id).as_deref(),
        Some(old.as_str())
    );
    // … so does `--only` it without `--replace`, and the ledger keeps the migrated record.
    let only = env
        .migrate(MigrateOptions {
            only: vec![w.roadmap.id],
            ..opts()
        })
        .await
        .unwrap();
    assert_eq!(row(&only, w.roadmap.id).status, Status::Failed, "{only}");
    let (status, digest, _) = env.ledger_row(w.roadmap.id).await.unwrap();
    assert_eq!(
        (status.as_str(), digest.as_deref()),
        ("migrated", Some(old.as_str()))
    );

    // … --replace re-imports every canvas from the legacy data.
    let replaced = env
        .migrate(MigrateOptions {
            replace: true,
            ..opts()
        })
        .await
        .unwrap();
    assert!(replaced.ok(), "{replaced}");
    assert_eq!(replaced.totals.replaced, 4);
    assert!(env
        .mock
        .with(|s| s.imports.iter().rev().take(4).all(|i| i.mode == "replace")));
    let r = row(&replaced, w.roadmap.id);
    assert_eq!(r.digest.as_deref(), Some(new.as_str()));
    assert_eq!(r.updates, 4);
    let (status, digest, detail) = env.ledger_row(w.roadmap.id).await.unwrap();
    assert_eq!(
        (status.as_str(), digest.as_deref()),
        ("migrated", Some(new.as_str()))
    );
    assert_eq!(detail["import_status"], "replaced");
    env.finish().await;
}

#[tokio::test]
async fn dry_run_reads_everything_and_sends_nothing() {
    let env = Env::new().await;
    let w = world(&env).await;
    let report = env
        .migrate(MigrateOptions {
            dry_run: true,
            ..opts()
        })
        .await
        .unwrap();
    assert!(report.ok(), "{report}");
    assert!(report.dry_run);
    assert_eq!(report.totals.dry_run, 4);
    assert_eq!(report.users.total, 4);
    assert_eq!(report.users.provisioned, 0);
    let r = row(&report, w.roadmap.id);
    assert_eq!(r.status, Status::DryRun);
    assert_eq!(
        (r.updates, r.members, r.invites, r.links, r.mentions),
        (3, 3, 4, 1, 2)
    );
    assert!(r.bytes > 0 && r.digest.is_none());
    assert_eq!(report.totals.updates, 3 + 2 + 41);
    env.mock.with(|s| {
        assert_eq!(s.stats_gets, 1, "only the preflight");
        assert_eq!((s.batch_posts, s.import_posts, s.digest_gets), (0, 0, 0));
    });
    assert_eq!(env.ledger().await, Vec::new(), "a dry run records nothing");
    assert!(report.table().contains("dry run"));

    // Without the ledger table a dry run still works; a real run refuses clearly.
    sqlx::query("DROP TABLE canvas_migrations")
        .execute(&env.db.pool)
        .await
        .unwrap();
    assert!(env
        .migrate(MigrateOptions {
            dry_run: true,
            ..opts()
        })
        .await
        .unwrap()
        .ok());
    let err = env.migrate(opts()).await.unwrap_err().to_string();
    assert!(err.contains("canvas_migrations"), "{err}");
    env.finish().await;
}

#[tokio::test]
async fn digest_mismatch_fails_and_is_recorded() {
    let env = Env::new().await;
    let w = world(&env).await;
    env.mock.with(|s| s.corrupt_digest.insert(w.personal.id));
    let report = env.migrate(opts()).await.unwrap();
    assert!(!report.ok());
    assert_eq!(report.totals.failed, 1);
    assert_eq!(report.totals.migrated, 3);
    let r = row(&report, w.personal.id);
    assert_eq!(r.status, Status::Failed);
    assert!(
        r.error.as_deref().unwrap().contains("digest mismatch"),
        "{r:?}"
    );
    let (status, digest, detail) = env.ledger_row(w.personal.id).await.unwrap();
    assert_eq!(status, "failed");
    assert_eq!(
        digest.as_deref(),
        Some(w.personal.truth().as_str()),
        "the import's digest is kept as evidence"
    );
    assert!(detail["error"]
        .as_str()
        .unwrap()
        .contains("digest mismatch"));
    assert!(report.table().trim_end().ends_with("FAILED"));
    // A later run does not mistake the unverified import for a migrated canvas.
    let again = env.migrate(opts()).await.unwrap();
    assert_eq!(row(&again, w.personal.id).status, Status::Failed);
    env.finish().await;
}

#[tokio::test]
async fn unknown_subject_fails_only_that_canvas() {
    let env = Env::new().await;
    let w = world(&env).await;
    // canvas-cloud "loses" carol after provisioning her (e.g. deleted between the calls).
    env.mock
        .with(|s| s.forget_subjects.insert(w.carol.to_string()));
    let report = env.migrate(opts()).await.unwrap();
    assert!(!report.ok());
    for id in [w.roadmap.id, w.busy.id] {
        let r = row(&report, id);
        assert_eq!(r.status, Status::Failed);
        let e = r.error.as_deref().unwrap();
        assert!(
            e.contains("422") && e.contains("unknown_subject") && e.contains(&w.carol.to_string()),
            "{e}"
        );
        assert!(env.mock.received(id).is_none(), "nothing stored");
    }
    assert_eq!(row(&report, w.personal.id).status, Status::Migrated);
    assert_eq!(row(&report, w.empty.id).status, Status::Migrated);
    assert_eq!(env.ledger_row(w.roadmap.id).await.unwrap().0, "failed");
    env.finish().await;
}

#[tokio::test]
async fn users_that_do_not_keep_their_id_stop_the_run() {
    let env = Env::new().await;
    let w = world(&env).await;
    let other = Uuid::now_v7();
    env.mock
        .with(|s| s.remap_user.insert(w.bob.to_string(), other));
    let report = env.migrate(opts()).await.unwrap();
    assert!(!report.ok());
    assert_eq!(report.users.failed.len(), 1);
    assert_eq!(report.users.failed[0].user_id, w.bob);
    assert!(report.users.failed[0].error.contains(&other.to_string()));
    assert!(report
        .error
        .as_deref()
        .unwrap()
        .contains("no canvas was migrated"));
    assert!(report.canvases.is_empty());
    assert_eq!(env.mock.with(|s| s.import_posts), 0);
    assert_eq!(env.ledger().await, Vec::new());
    assert!(report.table().contains("bob@example.com"));
    env.finish().await;
}

#[tokio::test]
async fn preflight_and_permission_errors_are_clear() {
    let env = Env::new().await;
    world(&env).await;
    let bad_key = env.migrate_with_key("cva_wrong", opts()).await.unwrap_err();
    let msg = format!("{bad_key:#}");
    assert!(
        msg.contains("/v1/apps/self/stats") && msg.contains("401"),
        "{msg}"
    );

    let nowhere = copper_cloud_canvas_migrate::run(
        &env.db.pool,
        &env.legacy.crypto,
        &env.client,
        &url::Url::parse("http://127.0.0.1:9/").unwrap(),
        APP_KEY,
        opts(),
    )
    .await
    .unwrap_err();
    assert!(format!("{nowhere:#}").contains("preflight"), "{nowhere:#}");

    env.mock.with(|s| s.allow_import = false);
    let forbidden = env.migrate(opts()).await.unwrap_err().to_string();
    assert!(forbidden.contains("allow_import"), "{forbidden}");
    assert_eq!(env.ledger().await, Vec::new());
    env.finish().await;
}

#[tokio::test]
async fn personal_canvases_follow_the_import_rules() {
    let env = Env::new().await;
    let w = world(&env).await;
    // Bob got an empty auto-created Personal in canvas-cloud: the legacy one replaces it.
    let mut bob_personal = env.legacy.canvas(w.bob, "Personal", "personal").await;
    let b = Client::new(505, &bob_personal.log);
    let u = b.edit(|d| {
        let shapes = d.get_or_insert_map("shapes");
        let mut t = d.transact_mut();
        shapes.insert(&mut t, "bp", any!({"type": "text"}));
    });
    env.legacy.append(&mut bob_personal, &u).await;
    let auto = env.mock.seed_personal(w.bob, false);
    // Alice already uses a non-empty Personal in canvas-cloud: refused without --replace.
    let used = env.mock.seed_personal(w.alice, true);

    let report = env.migrate(opts()).await.unwrap();
    assert_eq!(row(&report, bob_personal.id).status, Status::Migrated);
    assert!(env.mock.with(|s| !s.canvases.contains_key(&auto)));
    let r = row(&report, w.personal.id);
    assert_eq!(r.status, Status::Failed);
    assert!(
        r.error.as_deref().unwrap().contains("personal_exists"),
        "{r:?}"
    );

    let replaced = env
        .migrate(MigrateOptions {
            only: vec![w.personal.id],
            replace: true,
            ..opts()
        })
        .await
        .unwrap();
    assert!(replaced.ok(), "{replaced}");
    assert!(env.mock.with(|s| !s.canvases.contains_key(&used)));
    assert_eq!(
        env.mock.stored_digest(w.personal.id),
        Some(w.personal.truth())
    );
    env.finish().await;
}

#[tokio::test]
async fn unreadable_legacy_rows_fail_cleanly() {
    let env = Env::new().await;
    let w = world(&env).await;
    // A tampered update row on Busy, and a mention excerpt sealed under the wrong row id.
    sqlx::query(
        r#"UPDATE canvas_updates SET "update" = set_byte("update", 20, get_byte("update", 20) # 1)
           WHERE seq = (SELECT max(seq) FROM canvas_updates WHERE canvas_id = $1)"#,
    )
    .bind(w.busy.id)
    .execute(&env.db.pool)
    .await
    .unwrap();
    let wrong = copper_cloud_core::crypto::Crypto::seal(
        &w.roadmap.key,
        &copper_cloud_canvas_migrate::excerpt_aad(Uuid::nil()),
        b"lost",
    );
    sqlx::query("UPDATE canvas_mentions SET excerpt_sealed = $1 WHERE id = $2")
        .bind(&wrong)
        .bind(w.mention_ab)
        .execute(&env.db.pool)
        .await
        .unwrap();

    let report = env
        .migrate(MigrateOptions {
            concurrency: 4,
            ..opts()
        })
        .await
        .unwrap();
    assert!(!report.ok());
    let busy = row(&report, w.busy.id);
    assert_eq!(busy.status, Status::Failed);
    assert!(
        busy.error.as_deref().unwrap().contains("does not decrypt"),
        "{busy:?}"
    );
    assert!(
        env.mock.received(w.busy.id).is_none(),
        "an aborted body commits nothing"
    );
    // The undecryptable excerpt is sent empty with a warning, as the old server served it.
    let roadmap = row(&report, w.roadmap.id);
    assert_eq!(roadmap.status, Status::Migrated);
    assert_eq!(roadmap.warnings.len(), 1, "{roadmap:?}");
    let got = env.mock.received(w.roadmap.id).unwrap();
    let m = got
        .mentions
        .iter()
        .find(|m| m["id"] == w.mention_ab.to_string())
        .unwrap();
    assert_eq!(m["excerpt"], "");
    env.finish().await;
}

#[tokio::test]
async fn wrong_master_key_is_refused_before_anything_is_sent() {
    let env = Env::new().await;
    world(&env).await;
    let wrong = copper_cloud_core::crypto::Crypto::from_master_key(&[9u8; 32]);
    let err = copper_cloud_canvas_migrate::run(
        &env.db.pool,
        &wrong,
        &env.client,
        &env.mock.url,
        APP_KEY,
        opts(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("master_key"), "{err:#}");
    env.mock
        .with(|s| assert_eq!((s.batch_posts, s.import_posts), (0, 0)));
    // A dry run checks the same thing.
    let err = copper_cloud_canvas_migrate::run(
        &env.db.pool,
        &wrong,
        &env.client,
        &env.mock.url,
        APP_KEY,
        MigrateOptions {
            dry_run: true,
            ..opts()
        },
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("master_key"), "{err:#}");
    env.finish().await;
}

#[test]
fn chat_and_text_roots_survive_in_the_truth() {
    // Sanity check of the fixture itself: a reader that types its roots sees the content.
    let doc = Doc::new();
    let chat = doc.get_or_insert_array("chat");
    {
        let mut t = doc.transact_mut();
        chat.push_back(&mut t, any!({"id": "x"}));
    }
    assert!(common::digest::canonical_json(&doc.transact()).contains("\"chat\":[{\"id\":\"x\"}]"));
}

#[tokio::test]
async fn large_canvases_stream_with_bounded_concurrency() {
    let env = Env::new().await;
    let l = &env.legacy;
    let owner = l.user("big@example.com", "Big", false).await;
    // A canvas whose snapshot carries a 12 MiB image, then 300 small updates.
    let mut big = l.canvas(owner, "Big", "shared").await;
    let editor = Client::new(606, &big.log);
    let blob: Vec<u8> = (0..12 * 1024 * 1024_u32).map(|i| (i % 251) as u8).collect();
    let image = editor.edit(|d| {
        let assets = d.get_or_insert_map("assets");
        let mut t = d.transact_mut();
        assets.insert(&mut t, "img", Any::Buffer(blob.clone().into()));
    });
    let seq = l.append(&mut big, &image).await;
    l.compact(&big, seq).await;
    for i in 0..300 {
        let u = editor.edit(|d| {
            let shapes = d.get_or_insert_map("shapes");
            let mut t = d.transact_mut();
            shapes.insert(&mut t, format!("s{}", i % 50), any!({"x": i, "label": "x"}));
        });
        l.append(&mut big, &u).await;
    }
    // And 20 ordinary canvases.
    let mut small = Vec::new();
    for i in 0..20 {
        small.push(l.canvas(owner, &format!("Small {i}"), "shared").await.id);
    }

    let started = std::time::Instant::now();
    let report = env
        .migrate(MigrateOptions {
            concurrency: 3,
            ..opts()
        })
        .await
        .unwrap();
    assert!(report.ok(), "{report}");
    let r = row(&report, big.id);
    assert_eq!(r.updates, 301);
    assert!(r.bytes > 12 * 1024 * 1024);
    assert_eq!(r.digest.as_deref(), Some(big.truth().as_str()));
    assert_eq!(report.totals.migrated, 21);
    let ids: Vec<Uuid> = report.canvases.iter().map(|c| c.id).collect();
    assert_eq!(ids[0], big.id);
    assert_eq!(&ids[1..], small.as_slice(), "report keeps legacy order");
    println!(
        "migrated 21 canvases (12 MiB snapshot) in {:?}",
        started.elapsed()
    );
    env.finish().await;
}

#[tokio::test]
async fn dry_run_warns_about_lines_over_the_default_limit() {
    let env = Env::new().await;
    let l = &env.legacy;
    let owner = l.user("huge@example.com", "Huge", false).await;
    let mut huge = l.bare_canvas(owner, "Huge", "shared").await;
    let editor = Client::new(707, &[]);
    // 25 MiB of binary → ~33.3 MiB of base64 on one line.
    let blob = vec![7_u8; 25 * 1024 * 1024];
    let image = editor.edit(|d| {
        let assets = d.get_or_insert_map("assets");
        let mut t = d.transact_mut();
        assets.insert(&mut t, "img", Any::Buffer(blob.into()));
    });
    l.append(&mut huge, &image).await;
    let report = env
        .migrate(MigrateOptions {
            dry_run: true,
            ..opts()
        })
        .await
        .unwrap();
    let r = row(&report, huge.id);
    assert_eq!(r.status, Status::DryRun);
    assert_eq!(r.warnings.len(), 1, "{r:?}");
    assert!(r.warnings[0].contains("32 MiB"), "{r:?}");
    env.finish().await;
}
