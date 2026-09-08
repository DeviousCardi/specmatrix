#!/usr/bin/env bash
# Lock down `main` and turn on the repository's security features.
#
# Run once, after the first push. Everything here is done through the GitHub
# API, so you can read exactly what it will change before running it.
#
#   REVIEWS=0 ./tools/protect-main.sh    # solo maintainer, see the note below
#   ./tools/protect-main.sh              # requires one approving review
set -euo pipefail

REPO="${REPO:-DeviousCardi/specmatrix}"
# A ruleset requiring one review means you cannot merge your own pull request.
# On a solo repository that is a lock with the key inside, so REVIEWS=0 keeps
# every other protection — no direct pushes, no force pushes, CI must pass —
# while letting the maintainer merge. Raise it to 1 the day a second maintainer
# exists.
REVIEWS="${REVIEWS:-1}"

echo "repository: $REPO"
echo "required approving reviews: $REVIEWS"
echo

# --- the branch ruleset -------------------------------------------------------
# The admin role is listed with bypass_mode "pull_request", not "always". That
# distinction is the whole design here:
#
#   * Direct pushes to main are refused for everyone, the owner included.
#   * The owner can merge their own pull request without a second approver.
#
# Without it a sole maintainer cannot merge their own work at all, and the only
# way out is to disable the ruleset — which is worse protection than a narrow,
# deliberate exemption. A contributor still needs the owner's approval, because
# nobody else has write access to merge with.
echo "==> applying the ruleset on the default branch"
jq -n --argjson reviews "$REVIEWS" '{
  name: "main is protected",
  target: "branch",
  enforcement: "active",
  conditions: { ref_name: { include: ["~DEFAULT_BRANCH"], exclude: [] } },
  bypass_actors: [
    # 5 is the repository admin role. "pull_request" scopes the exemption to
    # merging a pull request; it does not permit a direct push.
    { actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "pull_request" }
  ],
  rules: [
    # No deleting the branch, and no force pushes. A force push to a published
    # branch rewrites history other people have already read.
    { type: "deletion" },
    { type: "non_fast_forward" },
    # Every change arrives as a pull request.
    { type: "pull_request",
      parameters: {
        required_approving_review_count: $reviews,
        require_code_owner_review: ($reviews > 0),
        dismiss_stale_reviews_on_push: true,
        require_last_push_approval: ($reviews > 0),
        required_review_thread_resolution: true,
        allowed_merge_methods: ["squash", "rebase"]
      } },
    # CI must pass, and must have run against the current tip of the branch —
    # that is what `strict` means, and without it a pull request can go green
    # against a base it was never merged with.
    { type: "required_status_checks",
      parameters: {
        strict_required_status_checks_policy: true,
        required_status_checks: [
          { context: "check" },
          { context: "corpus" },
          { context: "audit" }
        ]
      } }
  ]
}' > /tmp/specmatrix-ruleset.json

if gh api "repos/$REPO/rulesets" --jq '.[].name' 2>/dev/null | grep -qx "main is protected"; then
  id=$(gh api "repos/$REPO/rulesets" --jq '.[] | select(.name=="main is protected") | .id')
  gh api -X PUT "repos/$REPO/rulesets/$id" --input /tmp/specmatrix-ruleset.json >/dev/null
  echo "    updated existing ruleset $id"
else
  gh api -X POST "repos/$REPO/rulesets" --input /tmp/specmatrix-ruleset.json >/dev/null
  echo "    created"
fi

# --- repository settings ------------------------------------------------------
echo "==> repository settings"
gh api -X PATCH "repos/$REPO" \
  -F delete_branch_on_merge=true \
  -F allow_merge_commit=false \
  -F allow_rebase_merge=true \
  -F allow_squash_merge=true \
  -F has_wiki=false \
  -F has_projects=false >/dev/null
echo "    merge commits off, branches deleted on merge"

# --- security features --------------------------------------------------------
# Secret scanning with push protection refuses a push that contains a
# recognised credential, which is the only one of these that stops a mistake
# before it is public rather than after.
echo "==> security features"
gh api -X PATCH "repos/$REPO" --input - >/dev/null <<'JSON'
{
  "security_and_analysis": {
    "secret_scanning": { "status": "enabled" },
    "secret_scanning_push_protection": { "status": "enabled" }
  }
}
JSON
echo "    secret scanning + push protection"

gh api -X PUT "repos/$REPO/vulnerability-alerts" >/dev/null
echo "    dependency alerts"
gh api -X PUT "repos/$REPO/automated-security-fixes" >/dev/null
echo "    automated security fixes"

# --- what actually applied ----------------------------------------------------
echo
echo "==> in effect on $REPO:"
gh api "repos/$REPO/rulesets" --jq '.[] | "    ruleset: \(.name) [\(.enforcement)]"'
gh api "repos/$REPO" --jq '
  "    secret scanning:  \(.security_and_analysis.secret_scanning.status)",
  "    push protection:  \(.security_and_analysis.secret_scanning_push_protection.status)",
  "    merge commits:    \(.allow_merge_commit)",
  "    delete on merge:  \(.delete_branch_on_merge)"'
echo
echo "Verify by trying to push directly to main; it must be refused."
