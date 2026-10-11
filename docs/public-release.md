# First public release

`publish:public` publishes only committed `team/dev` content. `--initial` creates a
parentless public root with the same tree; it never moves or resets local branches.
The old source history must stay separate from the new public history. All real
publication and branch migration commands below require explicit owner approval.

## Rollback before making changes

Keep the old `team/dev` tip in a new archive branch before migration; never overwrite
an existing archive or change `archive/pre-public`. If verification fails, stop
without moving `team/dev`. If a push reports a connection error, inspect remote
`main` before retrying: the push may already have succeeded. Do not force-push,
delete remote refs, or merge old history to repair the release.

If local migration must be undone before any new work, stop workers, require a clean
checkout, and verify `git diff --exit-code HEAD archive/pre-initial-1456 --` passes.
With explicit approval, `git reset --soft archive/pre-initial-1456` restores the
local old tip; verify an empty `git status --short`. This leaves public `main`
unchanged and normal publication blocked until the verified public-root migration
is completed again. After new work starts, stop and plan recovery separately.

## One operation sequence

1. Complete the UTC metadata fix and this procedure. Finish active workers and stop
   new jobs. Create `hakunokina/dex` as an empty GitHub repository without a README,
   license, or gitignore.
2. In the `team/dev` checkout, run `git status --short` and
   `git branch --show-current`: the index and worktree must be clean and the branch
   must be `team/dev`. Check that `DEX_PUBLIC_TEST_REMOTE` and Git routing/date
   environment overrides are absent.
3. Set the repository-local public identity and install the hooks:
   ```sh
   git config --local user.name hakunokina
   git config --local user.email hakunokina@users.noreply.github.com
   npm run hooks:install
   ```
   If an existing `core.hooksPath` differs, the installer stops; do not override it.
4. Run `node --test scripts/public-gates.test.mjs`. The owner must verify that
   `git ls-remote --refs https://github.com/hakunokina/dex.git` returns no refs.
5. Run `npm run publish:public -- --initial --dry-run`. Tree, identity, message and
   history scans must pass. **Do not use this SHA for migration**: it was not pushed,
   and the real run may produce a different commit.
6. With explicit approval, run `npm run publish:public -- --initial`. Check push
   success and record the real `root=R` output. On a connection error, do not blindly
   retry: remote `main` may already exist.
7. Fetch and verify the real remote root:
   ```sh
   git fetch --no-tags https://github.com/hakunokina/dex.git refs/heads/main
   git rev-parse FETCH_HEAD
   git rev-list --parents -n 1 FETCH_HEAD
   git diff --exit-code team/dev FETCH_HEAD --
   ```
   The fetched SHA must equal the real published R, the parent listing must contain
   only R, and the trees must match. Stop on any mismatch.
8. Save the old tip with `git branch archive/pre-initial-1456 team/dev`. If that
   branch already exists, stop; do not overwrite it. Leave `archive/pre-public`
   untouched.
9. With workers stopped, a clean checkout/index and explicit approval, run
   `git reset --soft R`, replacing R with the **verified literal SHA from step 7**.
   The identical tree preserves index/worktree content. Verify `git status --short`
   is empty and `git rev-parse HEAD` equals R. **Do not merge old worker branches**;
   recreate them from the new `team/dev` root.
10. Run `npm run publish:public -- --dry-run` to verify normal mode accepts the new
    ancestry. Make subsequent changes only on the new `team/dev` history; publish
    with `npm run publish:public`. Check the first public CI results and configure
    the required job checks/ruleset for subsequent pull requests.

## Monitoring

Treat failed scans, ancestry checks, root/tree comparisons or CI jobs as a stop
condition. The explicit CI public-gates test covers initial file-remote publication,
manual root migration, normal publication and UTC metadata. CI runs after a push;
check its result, but keep all local publication gates enabled before every push.
