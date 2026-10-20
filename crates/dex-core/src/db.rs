//! App-wide SQLite store (`<data_dir>/dex.db`).
//!
//! Layout: versioned migrations (PRAGMA user_version) + plain `tasks::*` functions that take a
//! `&Connection`. Hosts (Tauri commands, MCP, later `dexd`) call the same functions through
//! [`crate::Core`], which emits the same `tasks-changed` event.

use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// One connection for the whole process; clones share it.
#[derive(Clone)]
pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn new(conn: Connection) -> Self {
        Self(Arc::new(Mutex::new(conn)))
    }
}

/// Append-only. Each entry runs once, in order; its index+1 becomes `user_version`.
const MIGRATIONS: &[&str] = &[
    // 1: tasks
    "CREATE TABLE tasks (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        title       TEXT    NOT NULL,
        description TEXT    NOT NULL DEFAULT '',
        status      TEXT    NOT NULL DEFAULT 'todo' CHECK (status IN ('todo','doing','done')),
        agent_id    TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE INDEX tasks_status ON tasks(status);",
    // 2: teams + agents; tasks belong to a team. (FIX-5: the default-team seed moved out
    // of the migration into SEED_DEFAULT_TEAM — `open` runs it when `teams` is empty, `open_bare`
    // (dexd) never does.)
    "CREATE TABLE teams (
        id         TEXT    PRIMARY KEY,
        name       TEXT    NOT NULL,
        kind       TEXT    NOT NULL CHECK (kind IN ('main','service')),
        cwd        TEXT    UNIQUE,
        created_at INTEGER NOT NULL,
        CHECK (kind = 'main' OR cwd IS NULL)
    );
    CREATE TABLE agents (
        id      TEXT    PRIMARY KEY,
        team_id TEXT    NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        name    TEXT    NOT NULL,
        kind    TEXT    NOT NULL CHECK (kind IN ('leader','worker')),
        title   TEXT    NOT NULL DEFAULT '',
        model   TEXT    NOT NULL CHECK (model IN ('opus','sonnet','haiku')),
        shirt   TEXT    NOT NULL,
        hair    TEXT    NOT NULL,
        skin    TEXT    NOT NULL,
        sort    INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX agents_team ON agents(team_id, sort);
    ALTER TABLE tasks ADD COLUMN team_id TEXT REFERENCES teams(id) ON DELETE CASCADE;
    CREATE INDEX tasks_team ON tasks(team_id);
    UPDATE tasks SET team_id = 'default' WHERE EXISTS (SELECT 1 FROM teams WHERE id = 'default');",
    // 3: role catalog (rows come from `roles::seed`, which upserts after every migrate) +
    // agents.role_slug / agents.provider. `model` loses its opus|sonnet|haiku CHECK (providers,
    // DEX-13), which SQLite can only do by rebuilding the table. Nothing references `agents`
    // (tasks.agent_id is a plain column), so dropping it cascades nowhere.
    // role_slug has no FK on purpose: a role dropped from a later seed must not break old agents.
    "CREATE TABLE roles (
        slug           TEXT    PRIMARY KEY,
        name           TEXT    NOT NULL,
        category       TEXT    NOT NULL,
        description    TEXT    NOT NULL DEFAULT '',
        default_engine TEXT    NOT NULL DEFAULT 'claude',
        system_prompt  TEXT    NOT NULL,
        sort           INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE agents_v3 (
        id        TEXT    PRIMARY KEY,
        team_id   TEXT    NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        name      TEXT    NOT NULL,
        kind      TEXT    NOT NULL CHECK (kind IN ('leader','worker')),
        title     TEXT    NOT NULL DEFAULT '',
        provider  TEXT    NOT NULL DEFAULT 'anthropic',
        model     TEXT    NOT NULL,
        role_slug TEXT,
        shirt     TEXT    NOT NULL,
        hair      TEXT    NOT NULL,
        skin      TEXT    NOT NULL,
        sort      INTEGER NOT NULL DEFAULT 0
    );
    INSERT INTO agents_v3 (id, team_id, name, kind, title, model, shirt, hair, skin, sort)
        SELECT id, team_id, name, kind, title, model, shirt, hair, skin, sort FROM agents;
    DROP TABLE agents;
    ALTER TABLE agents_v3 RENAME TO agents;
    CREATE INDEX agents_team ON agents(team_id, sort);
    UPDATE agents SET role_slug = 'lead' WHERE kind = 'leader';
    UPDATE agents SET role_slug = 'frontend' WHERE id = 'kaan';
    UPDATE agents SET role_slug = 'backend'  WHERE id = 'ayla';
    UPDATE agents SET role_slug = 'qa'       WHERE id = 'bora';",
    // 4: delegations (DEX-4). Agent ids are plain columns (like tasks.agent_id): the agents table
    // is rebuilt by migrations and a deleted agent must not erase the history. `team_id` is the
    // requesting team; `service_team_id` is set when a service team does the work. `notified`
    // = the leader already got it as context.
    "CREATE TABLE delegations (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        from_agent      TEXT    NOT NULL,
        to_agent        TEXT    NOT NULL,
        team_id         TEXT    NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        service_team_id TEXT    REFERENCES teams(id) ON DELETE CASCADE,
        prompt          TEXT    NOT NULL,
        status          TEXT    NOT NULL DEFAULT 'queued'
                        CHECK (status IN ('queued','running','done','done_no_report','failed')),
        summary         TEXT,
        result_path     TEXT,
        task_id         INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
        notified        INTEGER NOT NULL DEFAULT 0,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL
    );
    CREATE INDEX delegations_to ON delegations(to_agent, status);
    CREATE INDEX delegations_from ON delegations(from_agent, status);",
    // 5 (DEX-21): `roles.default_engine` was a Supabase leftover — Dex only runs Claude Code and
    // nothing ever read the column (team template hardcodes opus/sonnet). Drop it.
    "ALTER TABLE roles DROP COLUMN default_engine;",
    // 6 (DEX-22): leader plans (tasks with dependencies, advanced by delegation events), member
    // proposals the patron approves, and one-shot notes for a leader's next hook context.
    // `depends_on` is a JSON array of item keys of the same plan. `leader` / agent ids are plain
    // columns, like delegations.
    "CREATE TABLE plans (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        team_id    TEXT    NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        leader     TEXT    NOT NULL,
        title      TEXT    NOT NULL DEFAULT '',
        status     TEXT    NOT NULL DEFAULT 'active' CHECK (status IN ('active','done','cancelled')),
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE plan_items (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        plan_id       INTEGER NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
        key           TEXT    NOT NULL,
        title         TEXT    NOT NULL,
        prompt        TEXT    NOT NULL,
        target        TEXT    NOT NULL,
        depends_on    TEXT    NOT NULL DEFAULT '[]',
        status        TEXT    NOT NULL DEFAULT 'waiting'
                      CHECK (status IN ('waiting','blocked','running','done','failed','cancelled')),
        task_id       INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
        delegation_id INTEGER REFERENCES delegations(id) ON DELETE SET NULL,
        attempts      INTEGER NOT NULL DEFAULT 0,
        UNIQUE (plan_id, key)
    );
    CREATE INDEX plans_status ON plans(status);
    CREATE TABLE member_proposals (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        team_id    TEXT    NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
        leader     TEXT    NOT NULL,
        role_slug  TEXT    NOT NULL,
        name       TEXT,
        model      TEXT    NOT NULL,
        provider   TEXT    NOT NULL,
        reason     TEXT    NOT NULL,
        status     TEXT    NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected')),
        agent_id   TEXT,
        created_at INTEGER NOT NULL,
        decided_at INTEGER
    );
    CREATE TABLE leader_notes (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        agent_id   TEXT    NOT NULL,
        text       TEXT    NOT NULL,
        notified   INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX leader_notes_agent ON leader_notes(agent_id, notified);",
    // 7 (DEX-19): when a finished delegation reached its leader, when the leader's turn with it
    // ended, when the patron opened the worker's pane. Additive; older rows stay NULL and
    // delegate::COLS derives them from `notified`.
    "ALTER TABLE delegations ADD COLUMN delivered_at INTEGER;
    ALTER TABLE delegations ADD COLUMN read_at INTEGER;
    ALTER TABLE delegations ADD COLUMN seen_at INTEGER;",
    // 8 (DEX-23): role → process-skill mapping as data. Only the four skills Timur approved;
    // a fifth skill later is one SKILL.md dir + one row per role here. `role_slug` has no FK
    // on purpose (same reason as `agents.role_slug`): a role dropped from a later seed must
    // not break the mapping.
    "CREATE TABLE role_skills (
        role_slug TEXT NOT NULL,
        skill     TEXT NOT NULL,
        PRIMARY KEY (role_slug, skill)
    );
    INSERT INTO role_skills (role_slug, skill) VALUES
        ('backend', 'systematic-debugging'),
        ('backend', 'test-driven-development'),
        ('backend', 'requesting-code-review'),
        ('frontend', 'systematic-debugging'),
        ('frontend', 'test-driven-development'),
        ('frontend', 'requesting-code-review'),
        ('data-engineer', 'systematic-debugging'),
        ('data-engineer', 'test-driven-development'),
        ('code-automation', 'systematic-debugging'),
        ('code-automation', 'test-driven-development'),
        ('devops', 'systematic-debugging'),
        ('qa', 'systematic-debugging'),
        ('code-review', 'requesting-code-review'),
        ('lead', 'one-three-one-rule'),
        ('pm', 'one-three-one-rule');",
    // 9 (DEX-24): a worker's question to its leader (`ask_leader`), tied to the delegation it
    // works on. The leader answers or escalates to the patron. `leader_delivered_at` = the
    // question reached the leader; `answer_delivered_at` = the answer reached the worker.
    // Agent ids are plain columns, like delegations. New table only: nothing existing changes.
    "CREATE TABLE questions (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        delegation_id       INTEGER NOT NULL REFERENCES delegations(id) ON DELETE CASCADE,
        worker              TEXT    NOT NULL,
        leader              TEXT    NOT NULL,
        team_id             TEXT    NOT NULL,
        question            TEXT    NOT NULL,
        options             TEXT    NOT NULL DEFAULT '[]',
        context             TEXT,
        status              TEXT    NOT NULL DEFAULT 'open' CHECK (status IN ('open','escalated','answered')),
        note                TEXT,
        answer              TEXT,
        answered_by         TEXT    CHECK (answered_by IN ('leader','patron')),
        leader_delivered_at INTEGER,
        answer_delivered_at INTEGER,
        created_at          INTEGER NOT NULL,
        updated_at          INTEGER NOT NULL
    );
    CREATE INDEX questions_worker ON questions(worker, status);
    CREATE INDEX questions_leader ON questions(leader, status);",
    // 10 (DEX-26): the CLI an agent runs. `codex` ignores `provider` (routing is claude-only) and
    // its `model` comes from the codex catalog. Existing rows stay claude.
    "ALTER TABLE agents ADD COLUMN engine TEXT NOT NULL DEFAULT 'claude' CHECK (engine IN ('claude','codex'));",
    // 11 (DEX-30): a bulk engine/model change waiting for the agent's running session to end.
    // All three NULL = nothing pending; `pending_provider` NULL on a codex target = keep the provider.
    "ALTER TABLE agents ADD COLUMN pending_engine TEXT CHECK (pending_engine IN ('claude','codex'));
    ALTER TABLE agents ADD COLUMN pending_provider TEXT;
    ALTER TABLE agents ADD COLUMN pending_model TEXT;",
    // 12 (DEX-31): app-wide settings, one row per key (value = JSON). A missing key = the
    // built-in default, so no seed rows. First user: agent resource limits (`settings.rs`).
    "CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
    // 13 (DEX-41): the patron's notes (`notes.rs`). `scope` = 'shared' or a team id — no FK so
    // one column holds both; a trigger drops a deleted team's note instead.
    "CREATE TABLE notes (
        scope      TEXT    PRIMARY KEY,
        body       TEXT    NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TRIGGER notes_team_deleted AFTER DELETE ON teams BEGIN
        DELETE FROM notes WHERE scope = OLD.id;
    END;",
    // 14 (S2b): backend-owned resumable agent sessions. A session is used only while the agent's
    // configured engine still matches, so switching Claude/Codex never resumes the other engine.
    "CREATE TABLE agent_sessions (
        agent_id   TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        engine     TEXT NOT NULL CHECK (engine IN ('claude','codex')),
        updated_at INTEGER NOT NULL
    );",
    // 15 (S5-B): the session's transcript file (hook's `transcript_path`) — the chat reads it.
    "ALTER TABLE agent_sessions ADD COLUMN transcript TEXT;",
    // 16 (DEX-17): motor zinciri. Tükenen sağlayıcılar + ajanın zincir tercihi ve geçici
    // sağlayıcısı. `active_provider`/`active_model` NULL = ayarlanan (asıl) değer geçerli.
    "CREATE TABLE provider_exhausted (
        provider TEXT PRIMARY KEY,
        reset_at INTEGER NOT NULL,
        reason   TEXT NOT NULL DEFAULT ''
    );
    ALTER TABLE agents ADD COLUMN chain_off INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE agents ADD COLUMN active_provider TEXT;
    ALTER TABLE agents ADD COLUMN active_model TEXT;",
    // 17 (DEX-17 devam): zincirin son halkası Codex. Migration runner `user_version` ile aynı
    // dosyada iki kez güvenli çalışır; değişiklik yalnız yeni kolon + yeni tablodur.
    // `agent_chain_origins`, yeni Codex oturumu agent_sessions satırını devralınca önceki Claude
    // resume kimliğini korur; kullanıcı "geri dön" dediğinde bağlam kaybolmaz.
    "ALTER TABLE agents ADD COLUMN active_engine TEXT CHECK (active_engine IN ('claude','codex'));
    CREATE TABLE agent_chain_origins (
        agent_id   TEXT PRIMARY KEY,
        engine     TEXT NOT NULL CHECK (engine IN ('claude','codex')),
        session_id TEXT,
        transcript TEXT
    );",
    // 18 (Görev #94): zincir sıfırlanınca otomatik geri dönüş. Çalışan ajanın dönüşü oturum
    // kapanana bekler (bu bayrak); boş ajan hemen döndürülür. Yalnız yeni kolon.
    "ALTER TABLE agents ADD COLUMN chain_return_pending INTEGER NOT NULL DEFAULT 0;",
    // 19 (Görev #144): a worker's job in its own git worktree + branch; the leader merges it.
    // Team switch (default on; a folder that is no git repository ignores it). Additive only.
    "ALTER TABLE teams ADD COLUMN worktrees INTEGER NOT NULL DEFAULT 1;
    CREATE TABLE IF NOT EXISTS delegation_worktrees (
        delegation_id INTEGER PRIMARY KEY REFERENCES delegations(id) ON DELETE CASCADE,
        agent_id      TEXT    NOT NULL,
        root          TEXT    NOT NULL,
        path          TEXT    NOT NULL,
        branch        TEXT    NOT NULL,
        base          TEXT    NOT NULL,
        junction      INTEGER NOT NULL DEFAULT 0,
        created_at    INTEGER NOT NULL,
        merged_at     INTEGER,
        removed_at    INTEGER
    );",
    // 20: provider-aware usage history; additive and independently idempotent.
    crate::cost::SCHEMA,
    // 21: current session pointer + last context snapshot; no usage history changes.
    crate::cost::CONTEXT_SCHEMA,
    // 22 (Görev #283): the patron's verbatim words travel with the job (delegate / plan item, so
    // plan_retry and queued starts keep them). Additive only; old rows read NULL = none.
    "ALTER TABLE delegations ADD COLUMN patron_words TEXT;
    ALTER TABLE plan_items ADD COLUMN patron_words TEXT;",
    // 23 (Görev #286): a worker branch lands only after a code review of its tip commit; a fix
    // job's branch starts on the reviewed job's. Additive only (new tables).
    "CREATE TABLE IF NOT EXISTS delegation_reviews (
        review_id  INTEGER PRIMARY KEY REFERENCES delegations(id) ON DELETE CASCADE,
        target_id  INTEGER NOT NULL,
        sha        TEXT    NOT NULL,
        verdict    TEXT,
        findings   TEXT,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS delegation_reviews_target ON delegation_reviews (target_id);
    CREATE TABLE IF NOT EXISTS delegation_fixes (
        delegation_id INTEGER PRIMARY KEY REFERENCES delegations(id) ON DELETE CASCADE,
        fix_of        INTEGER NOT NULL
    );",
    // 24 (Görev #289): GPU broker — the leader's request, the patron's grant (type set + image
    // budget, bound to one delegation) and the broker jobs sent under it. Additive only.
    "CREATE TABLE IF NOT EXISTS gpu_requests (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        team_id    TEXT    NOT NULL,
        leader     TEXT    NOT NULL,
        worker     TEXT    NOT NULL,
        types      TEXT    NOT NULL,
        reason     TEXT    NOT NULL,
        budget     INTEGER NOT NULL,
        status     TEXT    NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected')),
        created_at INTEGER NOT NULL,
        decided_at INTEGER
    );
    CREATE TABLE IF NOT EXISTS gpu_grants (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        request_id    INTEGER NOT NULL UNIQUE REFERENCES gpu_requests(id),
        team_id       TEXT    NOT NULL,
        worker        TEXT    NOT NULL,
        types         TEXT    NOT NULL,
        budget        INTEGER NOT NULL,
        used          INTEGER NOT NULL DEFAULT 0,
        delegation_id INTEGER,
        created_at    INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS gpu_grants_delegation ON gpu_grants (delegation_id);
    CREATE TABLE IF NOT EXISTS gpu_jobs (
        job_id        TEXT    PRIMARY KEY,
        grant_id      INTEGER NOT NULL REFERENCES gpu_grants(id),
        delegation_id INTEGER NOT NULL,
        recipe        TEXT    NOT NULL,
        images        INTEGER NOT NULL,
        cached        INTEGER NOT NULL,
        created_at    INTEGER NOT NULL
    );",
    // 25 (Görev #292): broker önbelleği global (X-Dex-Task'a bakmaz) — aynı job_id'ye birden çok
    // izin erişebilir: erişim ayrı (job_id, grant_id) ilişkisi. Ve Dex'in gördüğü istek anahtarı →
    // son job_id: bütçe yetmezken bilinen önbellek isteği rezervasyonsuz geçer. Additive; eski
    // sahiplik satırları erişime kopyalanır (OR IGNORE: tekrar koşu zararsız).
    "CREATE TABLE IF NOT EXISTS gpu_job_grants (
        job_id   TEXT    NOT NULL,
        grant_id INTEGER NOT NULL REFERENCES gpu_grants(id),
        PRIMARY KEY (job_id, grant_id)
    );
    INSERT OR IGNORE INTO gpu_job_grants (job_id, grant_id) SELECT job_id, grant_id FROM gpu_jobs;
    CREATE TABLE IF NOT EXISTS gpu_request_keys (
        req_key    TEXT    PRIMARY KEY,
        job_id     TEXT    NOT NULL,
        created_at INTEGER NOT NULL
    );",
    // 26 (#436): the agent's path slug, kept — set once, never re-derived from member order nor
    // changed by a rename (#463) (`slug::assign`; rows from before get theirs at open,
    // `slug::backfill`). Additive; NULLs don't clash in the index.
    "ALTER TABLE agents ADD COLUMN slug TEXT;
    CREATE UNIQUE INDEX IF NOT EXISTS agents_team_slug ON agents (team_id, slug);",
    // 27 (#671): the leader's per-job effort (medium|high|xhigh; NULL = none, as before) on the job
    // and the plan item; `effort_skipped` = the job went into the patron's own session, effort not
    // applied. Additive only; old rows read NULL / 0.
    "ALTER TABLE delegations ADD COLUMN effort TEXT;
    ALTER TABLE delegations ADD COLUMN effort_skipped INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE plan_items ADD COLUMN effort TEXT;",
    // 28 (#844): projects and pinned job/session scope. Prior migrations remain untouched.
    crate::projects::SCHEMA,
    // 29 (#855): waiting/retried plan items keep their creation-time binding. The runner
    // backfills only restartable items from their own project (#337/#338); terminal and
    // running history remains NULL. No historical job paths or prompts are rewritten.
    // The versioned runner
    // applies this once atomically; reopen is idempotent. Before commit: ROLLBACK.
    // After commit: keep the additive columns (no DROP/user_version downgrade).
    "ALTER TABLE plan_items ADD COLUMN project_root TEXT;
    ALTER TABLE plan_items ADD COLUMN merge_branch TEXT;
    ALTER TABLE plan_items ADD COLUMN repo_key TEXT;",
    // 30 (#1147): independent of crew membership. The versioned transaction makes reopen
    // idempotent; existing and new teams default to full. Before commit: ROLLBACK.
    // After commit: keep the additive column; restore preferences with team_update(full).
    "ALTER TABLE teams ADD COLUMN test_level TEXT NOT NULL DEFAULT 'full' CHECK(test_level IN ('full','light'));",
    // 31 (#1490): additive role pools. Before commit: ROLLBACK.
    // After commit: keep tables; drain/cancel pending jobs before reverting the application.
    crate::delegate::ROLE_QUEUE_SCHEMA,
    // 32 (#1003): additive job model approvals; versioned runner is idempotent.
    crate::job_model::SCHEMA,
    // 33 (#1504): preserve the catalogue route across pending plan dependencies/retries.
    crate::job_model::PLAN_ROUTE_SCHEMA,
    // 34 (#1514): additive, idempotent leader instructions. Before commit: ROLLBACK.
    // After commit: retain the table and pending notes; revert only the application.
    crate::worker_notes::SCHEMA,
    // 35 (#1657): team purpose is independent of main/service. Versioned transaction
    // makes reopening idempotent; existing teams keep dev behavior. Before commit: ROLLBACK.
    // After commit: retain column/education roles, revert only application (no data deletion).
    "ALTER TABLE teams ADD COLUMN profile TEXT NOT NULL DEFAULT 'dev' CHECK(profile IN ('dev','edu'));",
    // 36 (#1697): host-only inbox; receipt metadata has no shared content.
    crate::inbox::SCHEMA,
    // 37 (#1710): the Codex engine and the gateway provider are gone. Data only: no column, table
    // or CHECK changes (the old CHECKs still admit 'codex'; the app refuses it). Versioned
    // transaction: before commit ROLLBACK. After commit nothing to restore: the deleted rows were
    // Codex resume pointers, the chain origins of Codex moves and quota marks of removed targets —
    // no code reads them any more; history (delegations, usage) is untouched.
    CODEX_REMOVAL,
    // 38 (#1730): free-standing pending proposals for Codex/gateway (not tied to a job — those
    // 37 already closed) could never be approved and held the pending quota. Data only, versioned
    // transaction: before commit ROLLBACK. After commit: rows stay (status/decided_at only);
    // to reopen: UPDATE member_proposals SET status='pending', decided_at=NULL WHERE id IN (...).
    STALE_PROPOSALS,
    // 39 (#1718): share replies + FCM device tokens, both host-only. Additive, IF NOT EXISTS.
    // Before commit: ROLLBACK. After commit: keep the tables; revert only the application.
    crate::inbox_reply::SCHEMA,
    // 40 (İş #1787): kesilen worker'ın bekleyen sorusu `cancelled` olur; bloklanan ask_leader /
    // wait_answer hemen kapanır, lidere iptal notu gider, geç cevap reddedilir. SQLite CHECK'i
    // yalnız tabloyu yeniden kurarak değişir (migration 3 deseni); questions'a başvuran FK yok,
    // veri aynen taşınır. Versioned runner tek sefer uygular, tekrar açılış idempotent.
    // Before commit: ROLLBACK. After commit: tabloyu tut, yalnız uygulamayı geri al.
    "CREATE TABLE questions_v40 (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        delegation_id       INTEGER NOT NULL REFERENCES delegations(id) ON DELETE CASCADE,
        worker              TEXT    NOT NULL,
        leader              TEXT    NOT NULL,
        team_id             TEXT    NOT NULL,
        question            TEXT    NOT NULL,
        options             TEXT    NOT NULL DEFAULT '[]',
        context             TEXT,
        status              TEXT    NOT NULL DEFAULT 'open' CHECK (status IN ('open','escalated','answered','cancelled')),
        note                TEXT,
        answer              TEXT,
        answered_by         TEXT    CHECK (answered_by IN ('leader','patron')),
        leader_delivered_at INTEGER,
        answer_delivered_at INTEGER,
        created_at          INTEGER NOT NULL,
        updated_at          INTEGER NOT NULL
    );
    INSERT INTO questions_v40 (id, delegation_id, worker, leader, team_id, question, options, context,
        status, note, answer, answered_by, leader_delivered_at, answer_delivered_at, created_at, updated_at)
        SELECT id, delegation_id, worker, leader, team_id, question, options, context,
        status, note, answer, answered_by, leader_delivered_at, answer_delivered_at, created_at, updated_at FROM questions;
    DROP TABLE questions;
    ALTER TABLE questions_v40 RENAME TO questions;
    CREATE INDEX questions_worker ON questions(worker, status);
    CREATE INDEX questions_leader ON questions(leader, status);",
    // 41 (#1858): the patron's daily three lines, one row per local day. Additive, IF NOT EXISTS.
    // Before commit: ROLLBACK. After commit: keep the table; revert only the application.
    crate::motivation::SCHEMA,
    // 42 (Plan #114): the leader's appends, prompt-part overrides, the applied print. Additive,
    // IF NOT EXISTS. Before commit: ROLLBACK. After commit: keep the tables; revert the application.
    crate::prompts::SCHEMA,
    // 43 (#2048): a leader's cancel/reassign of a job not started yet, waiting for the patron.
    // Additive, IF NOT EXISTS. Before commit: ROLLBACK. After commit: keep the table; revert only
    // the application (reject the pending cards, see job_change::SCHEMA).
    crate::job_change::SCHEMA,
    // 44 (#2104): a closed card that is not "done" work — the leader's patron-approved cancel, or
    // a failed/unreported card a same-titled new card replaced. Additive column + index + trigger;
    // the one-time UPDATE closes the leader-confirmed stale cards (guarded, a second run changes nothing).
    // Revert: `UPDATE tasks SET status='todo', closed_reason=NULL WHERE closed_reason IS NOT NULL;
    // DROP TRIGGER tasks_reopen; DROP INDEX delegations_task;` (the column stays, NULL).
    TASK_CLOSED,
    // 45 (Plan #124): "sen yokken" imleci + takım kapsamlı catchup sorgu indeksleri. Additive,
    // IF NOT EXISTS; versioned runner tek sefer uygular, tekrar açılış idempotent.
    // Before commit: ROLLBACK. After commit: keep the table/indexes; revert only the application
    // (full undo in catchup::SCHEMA).
    crate::catchup::SCHEMA,
    // 46 (#2236): two more approved process skills — plan-check (lead), eval-leak-check
    // (explorer, qa). Additive; INSERT OR IGNORE makes a second run a no-op. Before commit:
    // ROLLBACK. After commit, full undo: DELETE FROM role_skills WHERE skill IN
    // ('plan-check','eval-leak-check'); (the table and the other rows stay).
    PROCESS_SKILLS_46,
];

/// Migration 46. Idempotent on its own: the (role_slug, skill) PK + OR IGNORE
/// makes a second run change nothing.
const PROCESS_SKILLS_46: &str = "INSERT OR IGNORE INTO role_skills (role_slug, skill) VALUES
    ('lead', 'plan-check'),
    ('explorer', 'eval-leak-check'),
    ('qa', 'eval-leak-check');";

const TASK_CLOSED: &str = "ALTER TABLE tasks ADD COLUMN closed_reason TEXT CHECK (closed_reason IN ('cancelled','superseded'));
    CREATE INDEX IF NOT EXISTS delegations_task ON delegations(task_id);
    CREATE TRIGGER IF NOT EXISTS tasks_reopen AFTER UPDATE OF status ON tasks
    WHEN NEW.status <> 'done' AND NEW.closed_reason IS NOT NULL
    BEGIN UPDATE tasks SET closed_reason = NULL WHERE id = NEW.id; END;
    -- #2110: only the cards the leader confirmed by hand (2026-10-16), no general rule: #2056 → #2057
    -- merged, #2077's fix landed as b6c5245, the embedding job and #2076 were cancelled by the patron.
    -- Each guarded by its job link, so another DB changes nothing.
    UPDATE tasks SET status = 'done', closed_reason = CASE WHEN id IN (1258, 1483) THEN 'cancelled' ELSE 'superseded' END,
        updated_at = CAST(strftime('%s','now') AS INTEGER) * 1000
    WHERE status = 'todo' AND (
        (id = 1462 AND EXISTS (SELECT 1 FROM delegations WHERE id = 2056 AND task_id = 1462 AND status = 'failed'))
        OR (id = 1484 AND EXISTS (SELECT 1 FROM delegations WHERE id = 2077 AND task_id = 1484 AND status = 'done_no_report'))
        OR (id = 1258 AND EXISTS (SELECT 1 FROM delegations WHERE id = 1853 AND task_id = 1258 AND status = 'done'))
        OR (id = 1483 AND EXISTS (SELECT 1 FROM delegations d JOIN job_changes j ON j.delegation_id = d.id
            WHERE d.id = 2076 AND d.task_id = 1483 AND d.status = 'failed' AND j.action = 'cancel')));";

/// Migration 38. Idempotent on its own: a second run finds no pending match.
const STALE_PROPOSALS: &str = "
UPDATE member_proposals SET status = 'rejected',
    decided_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
  WHERE status = 'pending' AND provider IN ('codex', 'gateway')
    AND id NOT IN (SELECT proposal_id FROM job_models WHERE proposal_id IS NOT NULL);
";

/// Migration 37. Agents still on Codex or the gateway move to Claude Code (anthropic,
/// `CLAUDE_MID`, spelled out: a migration is frozen text — a test pins it); their transient chain
/// state, Codex resume rows and Codex-move chain origins go; chain/news settings drop the removed ids;
/// a waiting plan item or a job awaiting model approval on a removed route loses that request.
const CODEX_REMOVAL: &str = "
UPDATE agents SET engine = 'claude', provider = 'anthropic', model = 'claude-sonnet-5-5'
  WHERE engine = 'codex' OR provider = 'gateway';
UPDATE agents SET pending_engine = NULL, pending_provider = NULL, pending_model = NULL
  WHERE pending_engine = 'codex' OR pending_provider = 'gateway';
DELETE FROM agent_chain_origins
  WHERE engine = 'codex' OR agent_id IN (SELECT id FROM agents WHERE active_engine = 'codex');
UPDATE agents SET active_engine = NULL, active_provider = NULL, active_model = NULL, chain_return_pending = 0
  WHERE active_engine = 'codex' OR active_provider = 'gateway';
DELETE FROM agent_sessions WHERE engine = 'codex';
DELETE FROM provider_exhausted WHERE provider IN ('codex', 'gateway');
UPDATE settings SET value = (SELECT json_group_array(value) FROM
    (SELECT value FROM json_each(settings.value) WHERE value NOT IN ('codex', 'gateway') ORDER BY key))
  WHERE key IN ('chain_order', 'chain_disabled') AND json_valid(value) AND json_type(value) = 'array';
UPDATE settings SET value = json_remove(value, '$.codex')
  WHERE key = 'models_checked' AND json_valid(value) AND json_type(value) = 'object';
UPDATE settings SET value = (SELECT json_group_array(value) FROM
    (SELECT value FROM json_each(settings.value) WHERE value NOT LIKE 'codex:%' ORDER BY key))
  WHERE key = 'models_seen' AND json_valid(value) AND json_type(value) = 'array';
UPDATE plan_items SET model = NULL, model_reason = NULL, model_engine = NULL, model_provider = NULL
  WHERE status IN ('waiting', 'blocked') AND (model_engine = 'codex' OR model_provider = 'gateway');
UPDATE tasks SET status = 'todo' WHERE id IN (SELECT d.task_id FROM delegations d JOIN job_models j ON j.delegation_id = d.id
  WHERE d.status = 'queued' AND d.model_approval = 'pending' AND (j.engine = 'codex' OR j.provider = 'gateway'));
UPDATE delegations SET status = 'failed', summary = 'Codex/gateway kaldırıldı: model isteği geçersiz',
    updated_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
  WHERE status = 'queued' AND model_approval = 'pending'
    AND id IN (SELECT delegation_id FROM job_models WHERE engine = 'codex' OR provider = 'gateway');
";

const PLAN_SNAPSHOT_GUARD: &str = "CREATE TRIGGER IF NOT EXISTS plan_items_project_snapshot
    BEFORE UPDATE OF project_id,project_root,merge_branch,repo_key ON plan_items
    WHEN (OLD.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id) OR NEW.project_root IS NOT OLD.project_root
      OR NEW.merge_branch IS NOT OLD.merge_branch OR NEW.repo_key IS NOT OLD.repo_key
    BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;";

#[cfg(test)]
mod task_closed_migration_tests {
    use super::*;

    /// #2104: stale cards close once (cancel / same-titled replacement / the leader-confirmed
    /// three), nothing else moves, no row is deleted; a second migrate changes nothing.
    #[test]
    fn closes_cancelled_and_replaced_cards_once_and_keeps_the_rest() {
        let mut c = Connection::open_in_memory().unwrap();
        let at = MIGRATIONS.iter().position(|sql| *sql == TASK_CLOSED).unwrap();
        for (i, sql) in MIGRATIONS[..at].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch(
            "INSERT INTO teams(id,name,kind,created_at) VALUES('other','o','main',1);
            INSERT INTO tasks(id,title,status,agent_id,created_at,updated_at,team_id) VALUES
                (1,'iptal','todo','kaan',1,1,'default'), (2,'gitleaks','todo','kaan',1,1,'default'),
                (3,'gitleaks','todo','kaan',2,2,'default'), (4,'elle','todo',NULL,1,1,'default'),
                (5,'bitti','done','kaan',1,1,'default'), (6,'sürüyor','doing','kaan',1,1,'default'),
                (1258,'embedding','todo','kaan',1,1,'default'), (1462,'çakışma','todo','kaan',1,1,'default'),
                (1483,'ikili','todo','kaan',1,1,'default'), (1484,'ikili','todo','kaan',1,1,'default'),
                (1485,'ikili','todo','kaan',1,1,'other');
            INSERT INTO member_proposals(id,team_id,leader,role_slug,model,provider,reason,created_at) VALUES
                (1,'default','timur','backend','opus','anthropic','r',1), (2,'default','timur','backend','opus','anthropic','r',1);
            INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,summary,task_id,created_at,updated_at) VALUES
                (10,'timur','kaan','default','p','failed','lider iptal etti (patron onayıyla): r',1,1,1),
                (11,'timur','kaan','default','p','failed','x',2,1,1),
                (12,'timur','kaan','default','p','done_no_report','x',3,1,1),
                (13,'timur','kaan','default','p','running',NULL,6,1,1),
                (1853,'timur','kaan','default','p','done','ara rapor',1258,1,1),
                (2056,'timur','kaan','default','p','failed','başlatılamadı',1462,1,1),
                (2076,'timur','kaan','default','p','failed','lider iptal etti (patron onayıyla): r',1483,1,1),
                (2077,'timur','kaan','default','p','done_no_report','x',1484,1,1);
            INSERT INTO job_changes(proposal_id,action,delegation_id,reason) VALUES(1,'cancel',10,'r'), (2,'cancel',2076,'r');",
        )
        .unwrap();
        let count = |c: &Connection| -> i64 { c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap() };
        let before = count(&c);
        migrate(&mut c).unwrap();
        for run in 0..2 {
            if run == 1 {
                migrate(&mut c).unwrap();
            }
            let rows: Vec<(i64, String, Option<String>)> = {
                let mut st = c.prepare("SELECT id,status,closed_reason FROM tasks ORDER BY id").unwrap();
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
                rows.collect::<Result<_, _>>().unwrap()
            };
            let at = |s: &str, r: Option<&str>| (s.to_string(), r.map(String::from));
            let want = [
                // #2110: no general rule — a cancelled or same-titled card off the confirmed list stays.
                (1, at("todo", None)), (2, at("todo", None)), (3, at("todo", None)),
                (4, at("todo", None)), (5, at("done", None)), (6, at("doing", None)),
                (1258, at("done", Some("cancelled"))), (1462, at("done", Some("superseded"))), (1483, at("done", Some("cancelled"))),
                (1484, at("done", Some("superseded"))), (1485, at("todo", None)),
            ];
            let got: Vec<(i64, (String, Option<String>))> = rows.into_iter().map(|(i, s, r)| (i, (s, r))).collect();
            assert_eq!(got, want.to_vec(), "run {run}");
            assert_eq!(count(&c), before, "no card is deleted");
        }
        println!("task_closed migration: tasks before/after={before}/{}; 2 runs", count(&c));
    }
}

#[cfg(test)]
mod job_model_migration_tests {
    use super::*;

    #[test]
    fn codex_removal_migration_moves_rows_to_claude_twice_and_clean_db() {
        assert!(CODEX_REMOVAL.contains(&format!("model = '{}'", crate::providers::CLAUDE_MID)));
        let mut c = Connection::open_in_memory().unwrap();
        let before_version = MIGRATIONS.iter().position(|sql| *sql == CODEX_REMOVAL).unwrap();
        for (i, sql) in MIGRATIONS[..before_version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch(r#"
            UPDATE agents SET engine='codex', provider='glm', model='gpt-5.5' WHERE id='kaan';
            UPDATE agents SET provider='gateway', model='gpt-gw' WHERE id='ayla';
            UPDATE agents SET active_engine='codex', active_model='gpt-chain', chain_return_pending=1,
                pending_engine='codex', pending_model='gpt-x' WHERE id='bora';
            INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at) VALUES('kaan','rollout-1','codex',1),('timur','claude-1','claude',1);
            INSERT INTO agent_chain_origins(agent_id,engine,session_id) VALUES('bora','claude','c-origin'),('timur','claude','keep');
            INSERT INTO provider_exhausted(provider,reset_at,reason) VALUES('codex',1,'kota'),('gateway',1,'kota'),('glm',1,'kota');
            INSERT INTO settings(key,value) VALUES('chain_order','["glm","codex","anthropic","gateway","muse"]'),
                ('chain_disabled','["codex","local"]'),('models_checked','{"codex":1,"glm":2}'),('models_seen','["codex:gpt-6","glm:glm-6"]');
            INSERT INTO plans(id,team_id,leader,created_at,updated_at) VALUES(1,'default','timur',1,1);
            INSERT INTO plan_items(plan_id,key,title,prompt,target,status,model,model_reason,model_engine,model_provider) VALUES
                (1,'A','A','p','kaan','waiting','gpt-5.5','r','codex','codex'),
                (1,'B','B','p','kaan','waiting','opus','r','claude','anthropic'),
                (1,'C','C','p','kaan','done','gpt-5.5','r','codex','codex');
            INSERT INTO tasks(id,title,status,created_at,updated_at,team_id) VALUES(7,'model job','doing',1,1,'default');
            INSERT INTO member_proposals(id,team_id,leader,role_slug,model,provider,reason,created_at) VALUES(5,'default','timur','backend','gpt-5.5','codex','r',1);
            INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,created_at,updated_at,task_id,model,model_approval)
                VALUES(9,'timur','kaan','default','is',1,1,7,'gpt-5.5','pending'),(10,'timur','ayla','default','keep',1,1,NULL,NULL,NULL);
            INSERT INTO job_models(delegation_id,proposal_id,engine,provider) VALUES(9,5,'codex','codex');
        "#).unwrap();
        let agent = |c: &Connection, id: &str| -> (String, String, String, Option<String>, Option<String>, i64) {
            c.query_row("SELECT engine,provider,model,active_engine,pending_engine,chain_return_pending FROM agents WHERE id=?1", [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).unwrap()
        };
        let setting = |c: &Connection, k: &str| -> String { c.query_row("SELECT value FROM settings WHERE key=?1", [k], |r| r.get(0)).unwrap() };
        let count = |c: &Connection, sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
        let agents_before = count(&c, "SELECT COUNT(*) FROM agents");
        migrate(&mut c).unwrap();
        for run in 0..2 {
            if run == 1 {
                migrate(&mut c).unwrap();
                c.execute_batch(CODEX_REMOVAL).unwrap();
            }
            let mid = crate::providers::CLAUDE_MID.to_string();
            assert_eq!(agent(&c, "kaan"), ("claude".into(), "anthropic".into(), mid.clone(), None, None, 0));
            assert_eq!(agent(&c, "ayla"), ("claude".into(), "anthropic".into(), mid, None, None, 0));
            assert_eq!(agent(&c, "bora"), ("claude".into(), "anthropic".into(), "sonnet".into(), None, None, 0));
            assert_eq!(agent(&c, "timur").2, "opus", "untouched");
            assert_eq!(count(&c, "SELECT COUNT(*) FROM agents"), agents_before);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM agent_sessions WHERE engine='codex'"), 0);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM agent_sessions WHERE agent_id='timur'"), 1);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM agent_chain_origins WHERE agent_id='bora'"), 0, "Codex move origin");
            assert_eq!(count(&c, "SELECT COUNT(*) FROM agent_chain_origins WHERE agent_id='timur'"), 1);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM provider_exhausted"), 1);
            assert_eq!(setting(&c, "chain_order"), r#"["glm","anthropic","muse"]"#);
            assert_eq!(setting(&c, "chain_disabled"), r#"["local"]"#);
            assert_eq!(setting(&c, "models_checked"), r#"{"glm":2}"#);
            assert_eq!(setting(&c, "models_seen"), r#"["glm:glm-6"]"#);
            let item = |k: &str| -> (Option<String>, Option<String>) {
                c.query_row("SELECT model,model_engine FROM plan_items WHERE key=?1", [k], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
            };
            assert_eq!(item("A"), (None, None), "waiting Codex route dropped");
            assert_eq!(item("B"), (Some("opus".into()), Some("claude".into())));
            assert_eq!(item("C"), (Some("gpt-5.5".into()), Some("codex".into())), "history stays");
            let job: (String, String) = c.query_row("SELECT status,model_approval FROM delegations WHERE id=9", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!(job, ("failed".into(), "rejected".into()));
            assert_eq!(count(&c, "SELECT COUNT(*) FROM member_proposals WHERE id=5 AND status='rejected'"), 1);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM tasks WHERE id=7 AND status='todo'"), 1);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM delegations WHERE id=10 AND status='queued'"), 1);
            assert_eq!(count(&c, "SELECT COUNT(*) FROM pragma_foreign_key_check"), 0);
        }
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 {
            migrate(&mut clean).unwrap();
            clean.execute_batch(CODEX_REMOVAL).unwrap();
        }
        assert_eq!(count(&clean, "SELECT COUNT(*) FROM agents"), 0);
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
    }

    #[test]
    fn stale_proposals_migration_rejects_only_free_codex_gateway_pending_twice() {
        let mut c = Connection::open_in_memory().unwrap();
        let before_version = MIGRATIONS.iter().position(|sql| *sql == STALE_PROPOSALS).unwrap();
        for (i, sql) in MIGRATIONS[..before_version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch(r#"
            INSERT INTO member_proposals(id,team_id,leader,role_slug,model,provider,reason,created_at) VALUES
                (1,'default','timur','backend','gw-1','gateway','r',1),
                (2,'default','timur','backend','gpt-5.5','codex','r',1),
                (3,'default','timur','backend','GLM-5.3','glm','r',1),
                (4,'default','timur','backend','gw-2','gateway','r',1);
            INSERT INTO member_proposals(id,team_id,leader,role_slug,model,provider,reason,created_at,status,decided_at)
                VALUES(5,'default','timur','backend','gw-3','gateway','r',1,'approved',7);
            INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,created_at,updated_at) VALUES(9,'timur','kaan','default','is',1,1);
            INSERT INTO job_models(delegation_id,proposal_id,engine,provider) VALUES(9,4,'claude','gateway');
        "#).unwrap();
        let row = |c: &Connection, id: i64| -> (String, Option<i64>) {
            c.query_row("SELECT status,decided_at FROM member_proposals WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };
        let total = |c: &Connection| -> i64 { c.query_row("SELECT COUNT(*) FROM member_proposals", [], |r| r.get(0)).unwrap() };
        let before = total(&c);
        migrate(&mut c).unwrap();
        let decided = row(&c, 1).1;
        for run in 0..2 {
            if run == 1 { c.execute_batch(STALE_PROPOSALS).unwrap(); }
            for id in [1, 2] {
                let (status, at) = row(&c, id);
                assert_eq!(status, "rejected", "{id}");
                assert!(at.is_some_and(|t| t > 1), "{id}");
            }
            assert_eq!(row(&c, 1).1, decided, "second run leaves decided_at");
            assert_eq!(row(&c, 3), ("pending".into(), None), "other provider");
            assert_eq!(row(&c, 4), ("pending".into(), None), "tied to a job");
            assert_eq!(row(&c, 5), ("approved".into(), Some(7)));
            assert_eq!(total(&c), before);
        }
        assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
    }

    #[test]
    fn inbox_migration_is_additive_idempotent_and_keeps_pending_rows() {
        let mut c=Connection::open_in_memory().unwrap();
        for (i,sql) in MIGRATIONS.iter().take(35).enumerate() { c.execute_batch(sql).unwrap(); c.pragma_update(None,"user_version",i+1).unwrap(); }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        let counts=|c:&Connection| (c.query_row("SELECT COUNT(*) FROM teams",[],|r|r.get::<_,i64>(0)).unwrap(),c.query_row("SELECT COUNT(*) FROM agents",[],|r|r.get::<_,i64>(0)).unwrap());
        let before=counts(&c);
        migrate(&mut c).unwrap();
        c.execute_batch("INSERT INTO inbox_receipts(id,team_id,agent_id,client_id,reply) VALUES(1,'default','ayla','share','{}'); INSERT INTO inbox_messages(receipt_id,agent_id,prompt) VALUES(1,'ayla','pending');").unwrap();
        for _ in 0..2 {
            migrate(&mut c).unwrap(); c.execute_batch(crate::inbox::SCHEMA).unwrap();
            assert_eq!(counts(&c),before);
            assert_eq!(c.query_row("SELECT prompt FROM inbox_messages",[],|r|r.get::<_,String>(0)).unwrap(),"pending");
            assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        }
        let mut clean=Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); clean.execute_batch(crate::inbox::SCHEMA).unwrap(); assert_eq!(counts(&clean),(0,0)); }
    }

    #[test]
    fn education_profile_migration_twice_preserves_existing_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        for (i, sql) in MIGRATIONS.iter().take(34).enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        let counts = |c: &Connection| -> (i64, i64) {
            (c.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap())
        };
        let before = counts(&c);
        for _ in 0..2 {
            migrate(&mut c).unwrap();
            crate::roles::seed(&c).unwrap();
            assert_eq!(counts(&c), before);
            assert_eq!(c.query_row("SELECT profile FROM teams WHERE id='default'", [], |r| r.get::<_, String>(0)).unwrap(), "dev");
            assert_eq!(c.query_row("SELECT COUNT(*) FROM roles WHERE category='education'", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 {
            migrate(&mut clean).unwrap();
            crate::roles::seed(&clean).unwrap();
            assert_eq!(clean.query_row("SELECT COUNT(*) FROM roles", [], |r| r.get::<_, i64>(0)).unwrap(), 23);
            assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
        }
        assert_eq!(counts(&clean), (0, 0));
        assert!(clean.execute("INSERT INTO teams(id,name,kind,created_at,profile) VALUES('bad','Bad','main',0,'unknown')", []).is_err());
        println!("education migration: before/after={before:?}; existing + clean DB 2 runs; FK=0");
    }

    #[test]
    fn cancelled_question_migration_twice_preserves_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        let v40 = MIGRATIONS.iter().position(|sql| sql.starts_with("CREATE TABLE questions_v40")).unwrap();
        for (i, sql) in MIGRATIONS.iter().take(v40).enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,created_at,updated_at)
            VALUES('timur','kaan','default','keep',1,1);
            INSERT INTO questions(delegation_id,worker,leader,team_id,question,created_at,updated_at)
            VALUES(1,'kaan','timur','default','keep?',1,1);").unwrap();
        let snapshot = |c: &Connection| -> (String, String) {
            (c.query_row("SELECT status FROM questions WHERE id=1", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT question FROM questions WHERE id=1", [], |r| r.get(0)).unwrap())
        };
        assert!(c.execute("UPDATE questions SET status='cancelled' WHERE id=1", []).is_err(), "old CHECK rejects cancelled");
        let before = snapshot(&c);
        for _ in 0..2 {
            migrate(&mut c).unwrap();
            assert_eq!(snapshot(&c), before);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        c.execute("UPDATE questions SET status='cancelled' WHERE id=1", []).unwrap();
        assert_eq!(c.query_row("SELECT status FROM questions WHERE id=1", [], |r| r.get::<_, String>(0)).unwrap(), "cancelled");
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); }
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
        println!("cancelled migration: preserved={before:?}; v39 + clean DB 2 runs; FK=0");
    }

    #[test]
    fn motivation_migration_twice_preserves_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        let version = MIGRATIONS.iter().position(|sql| *sql == crate::motivation::SCHEMA).unwrap();
        for (i, sql) in MIGRATIONS[..version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO leader_notes(agent_id,text,created_at) VALUES('timur','keep',1);
            INSERT INTO settings(key,value) VALUES('maintenance_day','\"2026-10-14\"');").unwrap();
        let snapshot = |c: &Connection| -> (i64, i64, i64) {
            (c.query_row("SELECT COUNT(*) FROM leader_notes", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap())
        };
        let before = snapshot(&c);
        for _ in 0..2 { migrate(&mut c).unwrap(); assert_eq!(snapshot(&c), before); }
        crate::motivation::set(&c, "2026-10-15", "a", "b", "c", 1).unwrap();
        for _ in 0..2 {
            c.execute_batch(crate::motivation::SCHEMA).unwrap();
            assert_eq!(crate::motivation::get(&c, "2026-10-15").unwrap().unwrap().evening, "c");
        }
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); }
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
        println!("motivation migration: preserved={before:?}; v{version} + clean DB 2 runs");
    }

    #[test]
    fn process_skills_migration_twice_preserves_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        let version = MIGRATIONS.iter().position(|sql| *sql == PROCESS_SKILLS_46).unwrap();
        for (i, sql) in MIGRATIONS[..version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        let count = |c: &Connection| -> i64 { c.query_row("SELECT COUNT(*) FROM role_skills", [], |r| r.get(0)).unwrap() };
        let before = count(&c);
        assert_eq!(before, 15); // migration 8 rows, untouched by 46
        for _ in 0..2 {
            migrate(&mut c).unwrap();
            c.execute_batch(PROCESS_SKILLS_46).unwrap();
            assert_eq!(count(&c), before + 3);
        }
        for (role, skill) in [("lead", "plan-check"), ("explorer", "eval-leak-check"), ("qa", "eval-leak-check")] {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM role_skills WHERE role_slug=?1 AND skill=?2", [role, skill], |r| r.get(0)).unwrap();
            assert_eq!(n, 1, "{role}/{skill}");
        }
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); }
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
        assert_eq!(count(&clean), 18);
        println!("process skills migration: before/after={before}/{}; v{version} + clean DB 2 runs", count(&c));
    }

    #[test]
    fn prompts_migration_twice_preserves_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let version = MIGRATIONS.iter().position(|sql| *sql == crate::prompts::SCHEMA).unwrap();
        for (i, sql) in MIGRATIONS[..version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO notes(scope,body,updated_at) VALUES('default','takım notu',1);").unwrap();
        let snapshot = |c: &Connection| -> (i64, i64, String) {
            (c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT body FROM notes WHERE scope='default'", [], |r| r.get(0)).unwrap())
        };
        let before = snapshot(&c);
        for _ in 0..2 { migrate(&mut c).unwrap(); assert_eq!(snapshot(&c), before); }
        crate::roles::seed(&c).unwrap();
        crate::prompts::set(&c, "default", Some("sistem"), Some("mesaj")).unwrap();
        crate::prompts::part_set(&c, "role:lead", "özel").unwrap();
        crate::prompts::record_applied(&c, "timur", "fp").unwrap();
        for _ in 0..2 {
            c.execute_batch(crate::prompts::SCHEMA).unwrap();
            assert_eq!(crate::prompts::get(&c, "default").unwrap().system_append, "sistem");
            assert_eq!(c.query_row("SELECT COUNT(*) FROM prompt_overrides", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); }
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
        let fk: i64 = c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 0);
        println!("prompts migration: preserved={before:?}; v{version} + clean DB 2 runs; FK=0");
    }

    #[test]
    fn worker_notes_migration_twice_preserves_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        let before_version = MIGRATIONS.iter().position(|sql| *sql == crate::worker_notes::SCHEMA).unwrap();
        for (i,sql) in MIGRATIONS[..before_version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None,"user_version",i+1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,created_at,updated_at)
            VALUES('timur','kaan','default','keep',1,1);
            INSERT INTO tasks(title,created_at,updated_at,team_id) VALUES('keep',1,1,'default');").unwrap();
        let snapshot = |c: &Connection| -> (i64,i64,i64,String) {
            (c.query_row("SELECT COUNT(*) FROM delegations",[],|r|r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM tasks",[],|r|r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM agents",[],|r|r.get(0)).unwrap(),
             c.query_row("SELECT prompt FROM delegations WHERE id=1",[],|r|r.get(0)).unwrap())
        };
        let before = snapshot(&c);
        for _ in 0..2 { migrate(&mut c).unwrap(); assert_eq!(snapshot(&c),before); }
        c.execute("INSERT INTO worker_notes(delegation_id,text,created_at) VALUES(1,'keep note',1)",[]).unwrap();
        for _ in 0..2 {
            c.execute_batch(crate::worker_notes::SCHEMA).unwrap();
            assert_eq!(snapshot(&c),before);
            assert_eq!(c.query_row("SELECT text FROM worker_notes WHERE id=1",[],|r|r.get::<_,String>(0)).unwrap(),"keep note");
        }
        assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); clean.execute_batch(crate::worker_notes::SCHEMA).unwrap(); }
        assert_eq!(clean.query_row("SELECT COUNT(*) FROM worker_notes",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(clean.query_row("PRAGMA user_version",[],|r|r.get::<_,usize>(0)).unwrap(),MIGRATIONS.len());
        println!("worker notes migration: before/after={before:?}; existing + clean DB 2 runs; FK=0");
    }

    #[test]
    fn job_model_migration_twice_preserves_existing_rows_and_clean_db() {
        let mut c = Connection::open_in_memory().unwrap();
        let before_version = MIGRATIONS.iter().position(|sql| *sql == crate::job_model::SCHEMA).unwrap();
        for (i, sql) in MIGRATIONS[..before_version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,created_at,updated_at)
            VALUES('timur','kaan','default','keep',1,1);
            INSERT INTO plans(team_id,leader,title,status,created_at,updated_at) VALUES('default','timur','keep','active',1,1);
            INSERT INTO plan_items(plan_id,key,title,prompt,target,depends_on,status) VALUES(1,'A','keep','keep','kaan','[]','waiting');").unwrap();
        let snapshot = |c: &Connection| -> (i64, i64, i64, String) {
            (c.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM plan_items", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap(),
             c.query_row("SELECT prompt FROM delegations WHERE id=1", [], |r| r.get(0)).unwrap())
        };
        let before = snapshot(&c);
        for _ in 0..2 { migrate(&mut c).unwrap(); assert_eq!(snapshot(&c), before); }
        assert!(c.query_row("SELECT model IS NULL AND model_approval IS NULL FROM delegations WHERE id=1", [], |r| r.get::<_,bool>(0)).unwrap());
        assert_eq!(c.query_row("SELECT COUNT(*) FROM job_models", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 { migrate(&mut clean).unwrap(); }
        assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_,usize>(0)).unwrap(), MIGRATIONS.len());
        assert_eq!(clean.query_row("SELECT COUNT(*) FROM delegations", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    }

    #[test]
    fn plan_model_route_migration_twice_preserves_pending_plan_and_approved_job() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let version = MIGRATIONS.iter().position(|sql| *sql == crate::job_model::PLAN_ROUTE_SCHEMA).unwrap();
        for (i, sql) in MIGRATIONS[..version].iter().enumerate() {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO plans(team_id,leader,title,status,created_at,updated_at) VALUES('default','timur','keep','active',1,1);
            INSERT INTO plan_items(plan_id,key,title,prompt,target,depends_on,status,model,model_reason)
            VALUES(1,'A','keep','keep','kaan','[]','waiting','opus','keep');
            INSERT INTO delegations(from_agent,to_agent,team_id,prompt,created_at,updated_at,model,model_approval)
            VALUES('timur','kaan','default','keep',1,1,'opus','approved');
            INSERT INTO member_proposals(team_id,leader,role_slug,name,model,provider,reason,created_at,status)
            VALUES('default','timur','backend','Kaan','opus','anthropic','keep',1,'approved');
            INSERT INTO job_models(delegation_id,proposal_id,engine,provider) VALUES(1,1,'claude','anthropic');").unwrap();
        let counts = |c: &Connection| -> Vec<i64> {
            ["agents", "plans", "plan_items", "delegations", "member_proposals", "job_models"].iter()
                .map(|table| c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()).collect()
        };
        let before = counts(&c);
        for _ in 0..2 {
            migrate(&mut c).unwrap();
            assert_eq!(counts(&c), before);
            assert!(c.query_row("SELECT model='opus' AND model_reason='keep' AND model_engine IS NULL AND model_provider IS NULL FROM plan_items WHERE id=1",
                [], |r| r.get::<_, bool>(0)).unwrap(), "old route must not be inferred from today's worker");
            assert!(c.query_row("SELECT engine='claude' AND provider='anthropic' FROM job_models WHERE delegation_id=1",
                [], |r| r.get::<_, bool>(0)).unwrap());
            assert_eq!(c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        println!("plan model route migration: before/after={before:?}; 2 runs; FK=0");
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 {
            migrate(&mut clean).unwrap();
            assert_eq!(clean.query_row("PRAGMA user_version", [], |r| r.get::<_, usize>(0)).unwrap(), MIGRATIONS.len());
            assert_eq!(clean.query_row("SELECT COUNT(*) FROM plan_items", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
    }
}

pub fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    open_with(path, true)
}

/// FIX-5: `dexd` (sunucu) boş başlar — varsayılan takım tohumlanmaz; istemci boş host'ta
/// boş durum gösterir. Mevcut veriye dokunmaz (tohum yalnız `teams` boşken eklenir).
pub fn open_bare(path: &std::path::Path) -> rusqlite::Result<Connection> {
    open_with(path, false)
}

/// FIX-5: çıkarılan tohum — eskiden 2. migration'ın parçasıydı; migration metni değiştiği için
/// `teams` boşken çalışır (taze DB + tohumlu mod). Var olan DB'ler migration'ı çoktan koştu,
/// onlara dokunmaz.
const SEED_DEFAULT_TEAM: &str = "
    INSERT INTO teams (id, name, kind, cwd, created_at)
        VALUES ('default', 'Ana Takım', 'main', NULL, CAST(strftime('%s','now') AS INTEGER) * 1000);
    -- role_slug'ler eskiden 3. migration'daki UPDATE'lerden geliyordu (boş tabloya koşar, etkisiz);
    -- unvanlar eski metinle girer, `retitle_seed_crew` rol adına çevirir (DEX-15 akışı aynen korunur).
    INSERT INTO agents (id, team_id, name, kind, title, model, shirt, hair, skin, sort, role_slug) VALUES
        ('timur', 'default', 'Timur', 'leader', 'Takım Lideri', 'opus',   '#b3261e', '#1a1a1a', '#d9a878', 0, 'lead'),
        ('kaan',  'default', 'Kaan',  'worker', 'Geliştirici',  'sonnet', '#4a4f57', '#3b2a1e', '#e0b48c', 1, 'frontend'),
        ('ayla',  'default', 'Ayla',  'worker', 'Geliştirici',  'sonnet', '#5c6169', '#6b3a22', '#f0c8a0', 2, 'backend'),
        ('bora',  'default', 'Bora',  'worker', 'Geliştirici',  'sonnet', '#33373d', '#2a2a2a', '#c8956a', 3, 'qa');
    UPDATE tasks SET team_id = 'default' WHERE team_id IS NULL;";

fn open_with(path: &std::path::Path, seed_default_team: bool) -> rusqlite::Result<Connection> {
    crate::private_fs::sqlite(path).map_err(|e| rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN), Some(e.to_string()),
    ))?;
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    migrate_in(&mut conn, path.parent().unwrap_or_else(|| std::path::Path::new(".")))?;
    if seed_default_team {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0))?;
        if n == 0 {
            conn.execute_batch(SEED_DEFAULT_TEAM)?;
        }
    }
    crate::roles::seed(&conn)?;
    crate::roles::retitle_seed_crew(&conn)?;
    crate::projects::backfill(&conn, path.parent().unwrap_or_else(|| std::path::Path::new(".")))?;
    // #436: after the seed; a row it misses gets its slug on first need (`slug::assign`).
    if let Err(err) = crate::slug::backfill(&conn) {
        eprintln!("dex: slug ataması: {err}");
    }
    Ok(conn)
}

#[cfg(test)]
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    migrate_in(conn, std::path::Path::new("."))
}

fn migrate_in(conn: &mut Connection, data_dir: &std::path::Path) -> rusqlite::Result<()> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        // A must assign pre-v28 links before B reads an item's explicit project id.
        if i == 28 {
            crate::projects::backfill(conn, data_dir)?;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        if i == 28 {
            backfill_plan_snapshots(&tx, data_dir)?;
            tx.execute_batch(PLAN_SNAPSHOT_GUARD)?;
        }
        tx.pragma_update(None, "user_version", i + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Runs once, atomically with migration 29, before a Core/DB mutex exists. Missing,
/// archived or unavailable projects leave NULLs; no team/default/UI fallback is read.
fn backfill_plan_snapshots(conn: &Connection, data_dir: &std::path::Path) -> rusqlite::Result<()> {
    use crate::project_context::{self, ContextMode, ResolveRequest};
    let mut st = conn.prepare(
        "SELECT i.id,n.team_id,i.project_id FROM plan_items i JOIN plans n ON n.id=i.plan_id
        WHERE n.status='active' AND i.status IN ('waiting','blocked','failed') AND i.project_id IS NOT NULL",
    )?;
    let items = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    // One filesystem/Git validation per project, even if many old items share it.
    let mut checked = std::collections::HashMap::new();
    for (id, team, project) in items {
        let project = checked.entry((team.clone(), project.clone())).or_insert_with(|| {
            let context = project_context::resolve(
                conn,
                data_dir,
                ResolveRequest {
                    team_id: &team,
                    agent_id: None,
                    delegation_id: None,
                    project_id: Some(&project),
                    mode: ContextMode::Selection,
                },
            )
            .ok()?;
            let ready = project_context::validate(&context).ok()??;
            crate::projects::verify(conn, &team, &project, &ready).ok()
        });
        let Some(project) = project.as_ref() else { continue };
        conn.execute(
            "UPDATE plan_items SET project_root=?2,merge_branch=?3,repo_key=?4 WHERE id=?1",
            rusqlite::params![id, project.root_path, project.default_branch, project.repo_key],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod role_pool_migration_tests {
    use super::*;

    #[test]
    fn role_pool_schema_is_additive_and_replayable_twice() {
        let mut c = Connection::open_in_memory().unwrap();
        let pool_version = MIGRATIONS
            .iter()
            .position(|sql| *sql == crate::delegate::ROLE_QUEUE_SCHEMA)
            .unwrap();
        for (i, sql) in MIGRATIONS.iter().enumerate().take(pool_version) {
            c.execute_batch(sql).unwrap();
            c.pragma_update(None, "user_version", i + 1).unwrap();
        }
        c.execute_batch("INSERT INTO teams(id,name,kind,created_at) VALUES('test','Test','main',1);
                INSERT INTO tasks(title,description,team_id,created_at,updated_at) VALUES('old','verbatim','test',1,1);
                INSERT INTO delegations(from_agent,to_agent,team_id,prompt,created_at,updated_at) VALUES('leader','worker','test','original',1,1);").unwrap();
        let counts = |c: &Connection| {
            (
                c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                c.query_row("SELECT COUNT(*) FROM delegations", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            )
        };
        let before = counts(&c);
        for _ in 0..2 {
            migrate(&mut c).unwrap();
            c.execute_batch(crate::delegate::ROLE_QUEUE_SCHEMA).unwrap();
            assert_eq!(counts(&c), before);
            assert_eq!(
                c.query_row("SELECT to_agent,prompt FROM delegations", [], |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                )))
                .unwrap(),
                ("worker".into(), "original".into())
            );
            assert!(!c
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap());
        }
        println!("role pool migration: before/after tasks/jobs={before:?}; 2 runs; FK=0");
        let mut clean = Connection::open_in_memory().unwrap();
        for _ in 0..2 {
            migrate(&mut clean).unwrap();
            clean
                .execute_batch(crate::delegate::ROLE_QUEUE_SCHEMA)
                .unwrap();
            assert_eq!(counts(&clean), (0, 0));
            assert_eq!(
                clean
                    .query_row("SELECT COUNT(*) FROM delegation_role_queue", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }
}

#[cfg(test)]
mod project_migration_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sqlite_repairs_database_and_live_sidecars_without_losing_rows() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = crate::projects::tests::Fixture::new();
        let path = fixture.0.join("private.db");
        let first = open(&path).unwrap();
        first.execute_batch("CREATE TABLE privacy_fixture(value TEXT); INSERT INTO privacy_fixture VALUES('keep');").unwrap();
        let files = [path.clone(), path.with_file_name("private.db-wal"), path.with_file_name("private.db-shm")];
        for file in &files { std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap(); }
        for _ in 0..2 {
            let reopened = open(&path).unwrap();
            assert_eq!(reopened.query_row("SELECT COUNT(*) FROM privacy_fixture", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
            assert_eq!(reopened.query_row("SELECT value FROM privacy_fixture", [], |r| r.get::<_, String>(0)).unwrap(), "keep");
            for file in &files { assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600); }
        }
        drop(first);
    }

    #[test]
    fn pending_plan_upgrade_uses_its_own_project_and_old_failed_item_retries() {
        let f = crate::projects::tests::Fixture::new();
        let file = f.0.join("pending-v28.db");
        let c = Connection::open(&file).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for sql in &MIGRATIONS[..28] {
            c.execute_batch(sql).unwrap();
        }
        c.pragma_update(None, "user_version", 28).unwrap();
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        crate::projects::backfill(&c, &f.0).unwrap();
        let make = |name: &str| {
            let root = f.dir(name);
            let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready).unwrap()
        };
        let one = make("one");
        let missing = make("missing");
        crate::projects::set_default(&c, "default", Some(&missing.id)).unwrap();
        c.execute_batch("INSERT INTO plans(id,team_id,leader,created_at,updated_at) VALUES(1,'default','timur',1,1)").unwrap();
        for (id, status, attempts, project) in [
            (1, "waiting", 0, &one.id),
            (2, "failed", 2, &one.id),
            (3, "blocked", 0, &missing.id),
            (4, "done", 1, &one.id),
            (5, "running", 1, &one.id),
            (6, "cancelled", 0, &one.id),
        ] {
            c.execute("INSERT INTO plan_items(id,plan_id,key,title,prompt,target,status,attempts,project_id) VALUES(?1,1,?2,'old','old','backend',?3,?4,?5)",rusqlite::params![id,format!("K{id}"),status,attempts,project]).unwrap();
        }
        std::fs::rename(missing.root_path.as_ref().unwrap(), f.0.join("moved")).unwrap();
        drop(c);
        for _ in 0..2 {
            let c = open(&file).unwrap();
            for id in [1, 2] {
                let pin: (Option<String>, Option<String>, Option<String>) = c
                    .query_row("SELECT project_root,merge_branch,repo_key FROM plan_items WHERE id=?1", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                    })
                    .unwrap();
                assert_eq!(pin, (one.root_path.clone(), one.default_branch.clone(), one.repo_key.clone()));
            }
            for id in [3, 4, 5, 6] {
                assert_eq!(
                    c.query_row("SELECT project_root FROM plan_items WHERE id=?1", [id], |r| r.get::<_, Option<String>>(0)).unwrap(),
                    None
                );
            }
            assert_eq!(c.query_row("SELECT COUNT(*) FROM plan_items", [], |r| r.get::<_, i64>(0)).unwrap(), 6);
        }
        let c = open(&file).unwrap();
        let lead = crate::delegate::caller(&c, "timur").unwrap();
        crate::plan::retry(&c, &lead, 1, "K2").unwrap();
        let (jobs, _) = crate::plan::step(&c).unwrap();
        let retry = jobs.iter().find(|d| d.prompt.contains("görev K2:")).unwrap();
        assert_eq!(retry.project_root, one.root_path);
        assert_eq!(retry.merge_branch, one.default_branch);
        assert_eq!(retry.project_id, Some(one.id));
        // K3's project root was lost at upgrade: it fails closed instead of starting rootless.
        assert!(!jobs.iter().any(|d| d.prompt.contains("görev K3:")));
        let (status, job): (String, Option<i64>) =
            c.query_row("SELECT status, delegation_id FROM plan_items WHERE id = 3", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((status.as_str(), job), ("failed", None));
        let written: i64 = c.query_row("SELECT COUNT(*) FROM delegations WHERE prompt LIKE '%görev K3:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(written, 0);
        let told: String =
            c.query_row("SELECT text FROM leader_notes WHERE agent_id = 'timur' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!(told.contains("K3") && told.contains("project_unavailable") && told.contains("planı iptal edip yeniden oluştur"), "{told}");
    }

    #[test]
    fn plan_snapshots_upgrade_v28_and_reopen_twice_without_changing_old_items() {
        let f = crate::projects::tests::Fixture::new();
        let path = f.0.join("plan-v28.db");
        let mut c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for (i, sql) in MIGRATIONS.iter().take(28).enumerate() {
            let tx = c.transaction().unwrap();
            tx.execute_batch(sql).unwrap();
            tx.pragma_update(None, "user_version", i + 1).unwrap();
            tx.commit().unwrap();
        }
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch(
            "INSERT INTO plans(id,team_id,leader,title,created_at,updated_at) VALUES(1,'default','timur','old',1,1);
            INSERT INTO plan_items(id,plan_id,key,title,prompt,target) VALUES(1,1,'A','old','verbatim','backend');",
        )
        .unwrap();
        drop(c);
        for _ in 0..2 {
            let c = open(&path).unwrap();
            let old: (i64, String, Option<String>, Option<String>, Option<String>) = c
                .query_row("SELECT id,prompt,project_root,merge_branch,repo_key FROM plan_items", [], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .unwrap();
            assert_eq!(old, (1, "verbatim".into(), None, None, None));
            assert_eq!(c.query_row("SELECT COUNT(*) FROM plan_items", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
            assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), MIGRATIONS.len() as i64);
            assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
            assert!(c.execute("UPDATE plan_items SET project_root='rewrite' WHERE id=1", []).is_err());
        }
        let clean = f.0.join("plan-clean.db");
        for _ in 0..2 {
            let c = open(&clean).unwrap();
            assert_eq!(c.query_row("SELECT COUNT(*) FROM plan_items", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            c.prepare("SELECT project_root,merge_branch,repo_key FROM plan_items").unwrap();
        }
    }

    #[test]
    fn clean_project_schema_reopens_twice_bare_and_seeded() {
        let fixture=PathFixture::new();
        let file=fixture.0.join("clean.db");
        for _ in 0..2 {
            let c=open_bare(&file).unwrap();
            assert_eq!(c.query_row("SELECT COUNT(*) FROM teams",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            c.prepare("SELECT test_level FROM teams").unwrap();
            assert_eq!(c.query_row("SELECT COUNT(*) FROM projects",[],|r|r.get::<_,i64>(0)).unwrap(),0);
            assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
        }
        for _ in 0..2 {
            let c=open(&file).unwrap();
            assert_eq!(c.query_row("SELECT test_level FROM teams WHERE id='default'",[],|r|r.get::<_,String>(0)).unwrap(),"full");
            assert_eq!(c.query_row("SELECT COUNT(*) FROM projects",[],|r|r.get::<_,i64>(0)).unwrap(),1);
            assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
        }
    }

    #[test]
    fn v27_project_upgrade_preserves_rows_on_two_opens() {
        let fixture = PathFixture::new();
        let file = fixture.0.join("dex.db");
        let c = Connection::open(&file).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for sql in &MIGRATIONS[..27] {
            c.execute_batch(sql).unwrap();
        }
        c.pragma_update(None, "user_version", 27).unwrap();
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute_batch("INSERT INTO teams(id,name,kind,cwd,created_at) VALUES
            ('missing','Missing','main','Z:/dex-nonexistent-project-844',1),
            ('service','Service','service',NULL,1);
            INSERT INTO tasks(id,title,team_id,created_at,updated_at) VALUES(1,'old','default',1,1);
            INSERT INTO plans(id,team_id,leader,created_at,updated_at) VALUES(1,'default','timur',1,1);
            INSERT INTO delegations(id,from_agent,to_agent,team_id,service_team_id,prompt,status,result_path,task_id,created_at,updated_at)
            VALUES(1,'timur','service-worker','default','service','old','done','keep-result',1,1,1),
                  (2,'timur','kaan','default',NULL,'unknown','done',NULL,NULL,1,1);
            INSERT INTO plan_items(id,plan_id,key,title,prompt,target,task_id,delegation_id) VALUES(1,1,'a','old','old','backend',1,1);
            INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at)
            VALUES(1,'service-worker','historical-root','historical-worktree','old-branch','old-base',1);
            INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(2,1,'old-sha',1);
            INSERT INTO gpu_requests(id,team_id,leader,worker,types,reason,budget,created_at) VALUES(1,'default','timur','kaan','[]','old',1,1);
            INSERT INTO gpu_grants(id,request_id,team_id,worker,types,budget,delegation_id,created_at) VALUES(1,1,'default','kaan','[]',1,1,1);").unwrap();
        let tables = ["teams", "agents", "tasks", "plans", "plan_items", "delegations", "delegation_worktrees", "delegation_reviews", "gpu_grants"];
        let counts = |c: &Connection| tables.map(|t| c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap());
        let before = counts(&c);
        println!("migration28 before teams/agents/tasks/plans/items/jobs/worktrees/reviews/grants={before:?}");
        drop(c);
        for _ in 0..2 {
            let c = open(&file).unwrap();
            assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), MIGRATIONS.len() as i64);
            assert_eq!(counts(&c), before);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM teams WHERE test_level='full'", [], |r| r.get::<_, i64>(0)).unwrap(), before[0]);
            println!("project migrations after={:?}; version={}; projects=2; main=2; service-owned=0; FK=0", counts(&c), MIGRATIONS.len());
            assert_eq!(c.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM projects p JOIN teams t ON t.id=p.team_id WHERE t.kind='service'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
            assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
            let job: (String, String, String) = c.query_row("SELECT project_id,project_root,result_path FROM delegations WHERE id=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            assert_eq!(job, ("legacy-default".into(), "historical-root".into(), "keep-result".into()));
            assert_eq!(c.query_row("SELECT project_root FROM delegations WHERE id=2", [], |r| r.get::<_, Option<String>>(0)).unwrap(), None);
        }
    }

    #[test]
    fn v27_saved_sessions_keep_project_context_without_backfill() {
        use crate::project_context::{resolve, ContextMode, ResolveRequest};
        use crate::projects::{self, tests::Fixture, NewProject};

        let fixture = Fixture::new();
        let file = fixture.0.join("dex.db");
        let root = fixture.dir("legacy-repo");
        let c = Connection::open(&file).unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for sql in &MIGRATIONS[..27] {
            c.execute_batch(sql).unwrap();
        }
        c.pragma_update(None, "user_version", 27).unwrap();
        c.execute_batch(SEED_DEFAULT_TEAM).unwrap();
        c.execute(
            "UPDATE teams SET cwd=?1 WHERE id='default'",
            [root.to_str().unwrap()],
        )
        .unwrap();
        // Both leaders and workers had repo-root sessions before project bindings existed.
        c.execute_batch("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at) VALUES
            ('timur','old-leader','claude',1),('kaan','old-worker','claude',1);")
            .unwrap();
        drop(c);

        let c = open(&file).unwrap();
        let legacy = projects::get(&c, "default", "legacy-default").unwrap();
        let assert_resumed = |expected: Option<&str>, cwd: &std::path::Path| {
            for agent in ["timur", "kaan"] {
                let ctx = resolve(
                    &c,
                    &fixture.0,
                    ResolveRequest {
                        team_id: "default",
                        agent_id: Some(agent),
                        delegation_id: None,
                        project_id: None,
                        mode: ContextMode::Session,
                    },
                )
                .unwrap();
                assert_eq!(ctx.project_id.as_deref(), expected, "{agent}");
                assert_eq!(ctx.cwd, cwd, "{agent}");
                assert_eq!(ctx.project_root.as_deref(), expected.map(|_| cwd));
                assert!(ctx.delegation_id.is_none());
            }
        };
        assert_resumed(Some(&legacy.id), &root);
        // One bound active project is automatic even without a UI default.
        projects::set_default(&c, "default", None).unwrap();
        assert_resumed(Some(&legacy.id), &root);
        projects::set_default(&c, "default", Some(&legacy.id)).unwrap();

        let second_root = fixture.dir("second-repo");
        let second = projects::create(
            &c,
            "default",
            NewProject {
                name: "Second".into(),
                worktrees: None,
            },
            &projects::prepare_root(second_root.to_str().unwrap(), None).unwrap(),
        )
        .unwrap();
        assert_resumed(Some(&legacy.id), &root);
        projects::set_default(&c, "default", Some(&second.id)).unwrap();
        assert_resumed(Some(&second.id), &second_root);
        assert_eq!(
            resolve(
                &c,
                &fixture.0,
                ResolveRequest {
                    team_id: "default",
                    agent_id: Some("timur"),
                    delegation_id: None,
                    project_id: None,
                    mode: ContextMode::Selection,
                },
            )
            .unwrap_err()
            .code(),
            "project_required"
        );

        projects::set_default(&c, "default", None).unwrap();
        // The backfilled app-data placeholder is not a category root: the team presents it as
        // unset, while session resolution still falls back to that same folder.
        let team = crate::teams::get_team(&c, &fixture.0, "default").unwrap();
        assert!(team.workspace_root.is_none());
        let workspace = crate::teams::service_dir(&fixture.0, "default").join("workspace");
        assert_resumed(None, std::path::Path::new(&workspace));
        for p in [&legacy, &second] {
            projects::archive(&c, "default", &p.id, |_| false).unwrap();
        }
        assert_resumed(None, std::path::Path::new(&workspace));
        let counts: (i64, i64) = c.query_row(
            "SELECT COUNT(*),SUM(project_id IS NULL AND delegation_id IS NULL AND cwd IS NULL) FROM agent_sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(counts, (2, 2), "session bindings must not be backfilled");
    }

    struct PathFixture(std::path::PathBuf);
    impl PathFixture {
        fn new() -> Self {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/project-migration-844");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for PathFixture {
        fn drop(&mut self) { std::fs::remove_dir_all(&self.0).unwrap(); }
    }
}

pub mod tasks {
    use rusqlite::{params, Connection, OptionalExtension, Row};
    use serde::{Deserialize, Deserializer, Serialize};

    pub const STATUSES: &[&str] = &["todo", "doing", "done"];

    #[derive(Serialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct Task {
        pub id: i64,
        pub title: String,
        pub description: String,
        pub status: String,
        pub agent_id: Option<String>,
        pub team_id: Option<String>,
        pub project_id: Option<String>,
        /// #2104: the newest job of the card (its number on the board), from the DB.
        pub delegation_id: Option<i64>,
        /// #2104: `cancelled` / `superseded` on a closed (`done`) card that is not done work.
        pub closed_reason: Option<String>,
        pub created_at: i64,
        pub updated_at: i64,
    }

    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    pub struct NewTask {
        pub title: String,
        #[serde(default)]
        pub description: String,
        pub status: Option<String>,
        pub agent_id: Option<String>,
        pub team_id: Option<String>,
    }

    /// Absent field = unchanged. `agentId: null` clears the assignment.
    #[derive(Deserialize, Debug, Default)]
    #[serde(rename_all = "camelCase")]
    pub struct TaskPatch {
        pub title: Option<String>,
        pub description: Option<String>,
        pub status: Option<String>,
        #[serde(default, deserialize_with = "present")]
        pub agent_id: Option<Option<String>>,
    }

    fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(d).map(Some)
    }

    const COLS: &str = "id, title, description, status, agent_id, created_at, updated_at, team_id, project_id, (SELECT MAX(d.id) FROM delegations d WHERE d.task_id = tasks.id), closed_reason";

    fn row(r: &Row) -> rusqlite::Result<Task> {
        Ok(Task {
            id: r.get(0)?,
            title: r.get(1)?,
            description: r.get(2)?,
            status: r.get(3)?,
            agent_id: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
            team_id: r.get(7)?,
            project_id: r.get(8)?,
            delegation_id: r.get(9)?,
            closed_reason: r.get(10)?,
        })
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn check_status(s: &str) -> Result<(), String> {
        STATUSES.contains(&s).then_some(()).ok_or_else(|| format!("geçersiz durum: {s}"))
    }

    fn check_title(s: &str) -> Result<String, String> {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string()).ok_or_else(|| "başlık boş olamaz".into())
    }

    /// `team_id: None` = every team's tasks.
    pub fn list(conn: &Connection, team_id: Option<&str>) -> Result<Vec<Task>, String> {
        let mut st = conn
            .prepare(&format!(
                "SELECT {COLS} FROM tasks WHERE ?1 IS NULL OR team_id = ?1 ORDER BY created_at, id"
            ))
            .map_err(|e| e.to_string())?;
        let rows = st.query_map([team_id], row).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn get(conn: &Connection, id: i64) -> Result<Task, String> {
        conn.query_row(&format!("SELECT {COLS} FROM tasks WHERE id = ?1"), [id], row)
            .optional()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("görev yok: {id}"))
    }

    pub fn create(conn: &Connection, t: NewTask) -> Result<Task, String> {
        let title = check_title(&t.title)?;
        let status = t.status.unwrap_or_else(|| "todo".into());
        check_status(&status)?;
        let ts = now();
        conn.execute(
            "INSERT INTO tasks (title, description, status, agent_id, team_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![title, t.description, status, t.agent_id, t.team_id, ts],
        )
        .map_err(|e| e.to_string())?;
        get(conn, conn.last_insert_rowid())
    }

    pub fn update(conn: &Connection, id: i64, p: TaskPatch) -> Result<Task, String> {
        let mut t = get(conn, id)?;
        if let Some(v) = p.title {
            t.title = check_title(&v)?;
        }
        if let Some(v) = p.description {
            t.description = v;
        }
        if let Some(v) = p.status {
            check_status(&v)?;
            if v != "done" {
                t.closed_reason = None; // the `tasks_reopen` trigger clears the row
            }
            t.status = v;
        }
        if let Some(v) = p.agent_id {
            t.agent_id = v;
        }
        t.updated_at = now();
        conn.execute(
            "UPDATE tasks SET title = ?2, description = ?3, status = ?4, agent_id = ?5, updated_at = ?6
             WHERE id = ?1",
            params![id, t.title, t.description, t.status, t.agent_id, t.updated_at],
        )
        .map_err(|e| e.to_string())?;
        Ok(t)
    }

    pub fn delete(conn: &Connection, id: i64) -> Result<(), String> {
        match conn.execute("DELETE FROM tasks WHERE id = ?1", [id]) {
            Ok(0) => Err(format!("görev yok: {id}")),
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn mem() -> Connection {
            let mut c = Connection::open_in_memory().unwrap();
            super::super::migrate(&mut c).unwrap();
            // FIX-5: the default-team seed left migration 2; these task tests assume the old
            // seeded shape, so seed explicitly (production does it in `open`).
            c.execute_batch(super::super::SEED_DEFAULT_TEAM).unwrap();
            c
        }

        #[test]
        fn crud_roundtrip() {
            let c = mem();
            let t = create(&c, NewTask { title: "  a  ".into(), description: String::new(), status: None, agent_id: Some("timur".into()), team_id: Some("default".into()) }).unwrap();
            assert_eq!((t.title.as_str(), t.status.as_str(), t.agent_id.as_deref()), ("a", "todo", Some("timur")));
            assert_eq!(list(&c, Some("default")).unwrap().len(), 1);
            assert!(list(&c, Some("other")).unwrap().is_empty());

            let p: TaskPatch = serde_json::from_str(r#"{"status":"doing"}"#).unwrap();
            let u = update(&c, t.id, p).unwrap();
            assert_eq!((u.status.as_str(), u.agent_id.as_deref()), ("doing", Some("timur")));

            let p: TaskPatch = serde_json::from_str(r#"{"agentId":null}"#).unwrap();
            assert_eq!(update(&c, t.id, p).unwrap().agent_id, None);

            let p: TaskPatch = serde_json::from_str(r#"{"status":"nope"}"#).unwrap();
            assert!(update(&c, t.id, p).is_err());
            assert!(create(&c, NewTask { title: " ".into(), description: String::new(), status: None, agent_id: None, team_id: None }).is_err());

            assert_eq!(list(&c, None).unwrap().len(), 1);
            delete(&c, t.id).unwrap();
            assert!(delete(&c, t.id).is_err());
            assert!(list(&c, None).unwrap().is_empty());
        }

        #[test]
        fn migrate_is_idempotent() {
            let mut c = mem();
            super::super::migrate(&mut c).unwrap();
            let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, super::super::MIGRATIONS.len());
        }

        /// FIX-5: `open` tohumlar, `open_bare` (dexd) boş başlar; rol kataloğu ikisinde de dolu.
        /// İkinci açılış tohumlamaz (mevcut veriye dokunmaz).
        #[test]
        fn bare_open_starts_empty_seeded_open_does_not_reseed() {
            let count = |c: &Connection| -> i64 {
                c.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0)).unwrap()
            };
            let bare = super::super::open_bare(std::path::Path::new(":memory:")).unwrap();
            assert_eq!(count(&bare), 0);
            let roles: i64 = bare.query_row("SELECT COUNT(*) FROM roles", [], |r| r.get(0)).unwrap();
            assert!(roles > 0, "rol kataloğu sunucuda da dolu");

            let dir = std::env::temp_dir().join(format!("dex-db-seed-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("dex.db");
            let seeded = super::super::open(&file).unwrap();
            assert_eq!(count(&seeded), 1);
            drop(seeded);
            let again = super::super::open(&file).unwrap();
            assert_eq!(count(&again), 1, "ikinci açılış ikinci takım eklemez");
            drop(again);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A v2 database (DEX-7 shape, with user data) upgrades to v3 without losing rows.
        #[test]
        fn v2_to_v3_keeps_data() {
            let mut c = Connection::open_in_memory().unwrap();
            c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            for (i, sql) in super::super::MIGRATIONS[..2].iter().enumerate() {
                c.execute_batch(sql).unwrap();
                c.pragma_update(None, "user_version", i + 1).unwrap();
            }
            // FIX-5: v2 shape included the seeded default crew (migration 2 carried the INSERTs
            // then — no role_slug yet, that came with v3). Reproduce it literally, not via SEED_DEFAULT_TEAM.
            c.execute_batch(
                "INSERT INTO teams (id, name, kind, cwd, created_at)
                    VALUES ('default', 'Ana Takım', 'main', NULL, 5);
                 INSERT INTO agents (id, team_id, name, kind, title, model, shirt, hair, skin, sort) VALUES
                    ('timur', 'default', 'Timur', 'leader', 'Takım Lideri', 'opus',   '#b3261e', '#1a1a1a', '#d9a878', 0),
                    ('kaan',  'default', 'Kaan',  'worker', 'Geliştirici',  'sonnet', '#4a4f57', '#3b2a1e', '#e0b48c', 1),
                    ('ayla',  'default', 'Ayla',  'worker', 'Geliştirici',  'sonnet', '#5c6169', '#6b3a22', '#f0c8a0', 2),
                    ('bora',  'default', 'Bora',  'worker', 'Geliştirici',  'sonnet', '#33373d', '#2a2a2a', '#c8956a', 3);",
            )
            .unwrap();
            c.execute_batch(
                "INSERT INTO teams VALUES ('t2','İkinci','service',NULL,5);
                 INSERT INTO agents VALUES ('x-1','t2','Xen','leader','Şef','haiku','#111','#222','#333',0);
                 INSERT INTO agents VALUES ('y-1','t2','Yal','worker','','opus','#444','#555','#666',1);
                 UPDATE agents SET title = 'Özel' WHERE id = 'kaan';",
            )
            .unwrap();
            // Literal v2 fixture: the current task API now reads v28's additive project column.
            c.execute("INSERT INTO tasks(title,description,status,agent_id,team_id,created_at,updated_at) VALUES('eski','d','doing','y-1','t2',1,1)",[]).unwrap();
            let dump = |c: &Connection| -> Vec<String> {
                let mut st = c.prepare("SELECT id||team_id||name||kind||title||model||shirt||hair||skin||sort FROM agents ORDER BY id").unwrap();
                st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
            };
            let before = dump(&c);

            super::super::migrate(&mut c).unwrap();
            crate::roles::seed(&c).unwrap();

            let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, super::super::MIGRATIONS.len()); // v2 → latest (≥ 3)
            assert_eq!(dump(&c), before, "agent rows survive the rebuild");
            assert_eq!(list(&c, Some("t2")).unwrap()[0].agent_id.as_deref(), Some("y-1"));
            let roles: Vec<(String, Option<String>, String)> = c
                .prepare("SELECT id, role_slug, provider FROM agents ORDER BY id").unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
            let get = |id: &str| roles.iter().find(|r| r.0 == id).map(|r| (r.1.as_deref(), r.2.as_str())).unwrap();
            assert_eq!(get("timur"), (Some("lead"), "anthropic"));
            assert_eq!(get("x-1"), (Some("lead"), "anthropic"));
            assert_eq!(get("kaan"), (Some("frontend"), "anthropic"));
            assert_eq!(get("y-1"), (None, "anthropic"));
            // FK to teams still cascades; the model CHECK is gone.
            c.execute("UPDATE agents SET model = 'gpt-5' WHERE id = 'y-1'", []).unwrap();
            c.execute("DELETE FROM teams WHERE id = 't2'", []).unwrap();
            let n: i64 = c.query_row("SELECT COUNT(*) FROM agents WHERE team_id = 't2'", [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0);
            assert!(c.execute("INSERT INTO agents (id, team_id, name, kind, model, shirt, hair, skin) VALUES ('z','nope','Z','worker','m','a','b','c')", []).is_err());
            let fk: Vec<(String, String)> = c.prepare("PRAGMA foreign_key_check").unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
            assert!(fk.is_empty(), "{fk:?}");
        }

        /// A v4 database (with the DEX-12 `default_engine` column and user data) upgrades to v5:
        /// the column is gone, every row survives.
        #[test]
        fn v4_to_v5_drops_default_engine_keeps_data() {
            let mut c = Connection::open_in_memory().unwrap();
            c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            for (i, sql) in super::super::MIGRATIONS[..4].iter().enumerate() {
                c.execute_batch(sql).unwrap();
                c.pragma_update(None, "user_version", i + 1).unwrap();
            }
            crate::roles::seed(&c).unwrap();
            let before: Vec<String> = c
                .prepare("SELECT slug||name||category||description||system_prompt||sort FROM roles ORDER BY sort, slug")
                .unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            assert_eq!(before.len(), 23);

            super::super::migrate(&mut c).unwrap();

            let v: usize = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
            assert_eq!(v, super::super::MIGRATIONS.len());
            let cols: Vec<String> = c
                .prepare("SELECT name FROM pragma_table_info('roles') ORDER BY cid").unwrap()
                .query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            assert!(!cols.contains(&"default_engine".to_string()), "{cols:?}");
            let after: Vec<String> = c
                .prepare("SELECT slug||name||category||description||system_prompt||sort FROM roles ORDER BY sort, slug")
                .unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
            assert_eq!(after, before, "role rows survive the column drop");
            // Fresh seed still upserts into the narrower table.
            crate::roles::seed(&c).unwrap();
            assert_eq!(crate::roles::list(&c).unwrap().len(), 23);
        }
    }
}

pub fn with<T>(db: &Db, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
    let conn = db.0.lock().map_err(|_| "db kilidi zehirli".to_string())?;
    f(&conn)
}
