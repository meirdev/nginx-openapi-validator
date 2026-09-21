#!/usr/bin/env bash
#
# End-to-end test for the nginx OpenAPI validator module.
#
# Usage:
#   ./example/test.sh                  # builds and runs everything with Docker Compose
#   OAV_BASE_URL=http://host:port OAV_LOG_CMD='cat /path/access.log' ./example/test.sh
#                                      # runs the checks against an nginx you started yourself
#
# OAV_LOG_CMD is a shell command that prints the nginx access log; it is only
# needed for the $oav_* variable checks.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PORT=18088

GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

pass=0
fail=0

check() {
    local description="$1"
    local expected_status="$2"
    shift 2
    local actual_status
    actual_status=$(curl -s -o /dev/null -w "%{http_code}" "$@" 2>/dev/null || echo "000")

    if [ "$actual_status" = "$expected_status" ]; then
        echo -e "  ${GREEN}✓${NC} $description (HTTP $actual_status)"
        pass=$((pass + 1))
    else
        echo -e "  ${RED}✗${NC} $description (expected $expected_status, got $actual_status)"
        fail=$((fail + 1))
    fi
}

check_log() {
    local description="$1"
    local pattern="$2"
    if echo "$LOGS" | grep -q "$pattern"; then
        echo -e "  ${GREEN}✓${NC} $description"
        pass=$((pass + 1))
    else
        echo -e "  ${RED}✗${NC} $description (pattern '$pattern' not found)"
        fail=$((fail + 1))
    fi
}

echo "=== Nginx OpenAPI Validator - Integration Tests ==="
echo ""

if [ -z "${OAV_BASE_URL:-}" ]; then
    echo "Starting services with docker compose..."
    cd "$SCRIPT_DIR"
    docker compose up -d --build --wait 2>&1

    cleanup() {
        echo ""
        echo "Stopping services..."
        cd "$SCRIPT_DIR"
        docker compose down 2>/dev/null || true
    }
    trap cleanup EXIT

    BASE="http://localhost:$PORT"
    OAV_LOG_CMD="docker compose logs nginx 2>&1"
else
    BASE="$OAV_BASE_URL"
fi

# Wait for nginx to be ready
echo "Waiting for nginx at $BASE..."
for i in $(seq 1 20); do
    if curl -s -o /dev/null "$BASE/" 2>/dev/null; then
        break
    fi
    sleep 0.5
done

JSON='Content-Type: application/json'

# ── Path validation ──────────────────────────────────────────────────
echo ""
echo "Path validation:"
check "Valid path /api/pets passes"          200 "$BASE/api/pets?status=available"
check "Valid path /api/pets/42 passes"       200 "$BASE/api/pets/42"
check "Trailing slash is tolerated"          200 "$BASE/api/pets/?status=available"
check "Percent-encoded path param decodes"   200 "$BASE/api/pets/%34%32"
check "Unknown path /api/unknown → 404"     404 "$BASE/api/unknown"
check "Unknown nested path → 404"           404 "$BASE/api/foo/bar"

# ── Method validation ────────────────────────────────────────────────
echo ""
echo "Method validation:"
check "GET /api/pets is allowed"             200 "$BASE/api/pets?status=available"
check "DELETE /api/pets → 405"              405 -X DELETE "$BASE/api/pets"
check "PATCH /api/pets → 405"               405 -X PATCH "$BASE/api/pets"
check "DELETE /api/pets/42 is allowed"       200 -X DELETE "$BASE/api/pets/42"
check "HEAD falls back to GET"               200 -I "$BASE/api/pets?status=available"

# ── Path parameter validation ────────────────────────────────────────
echo ""
echo "Path parameter validation:"
check "Non-integer petId → 400"             400 "$BASE/api/pets/abc"

# ── Query parameter validation ───────────────────────────────────────
echo ""
echo "Query parameter validation:"
check "Missing required 'status' → 400"    400 "$BASE/api/pets"
check "Valid status=available passes"        200 "$BASE/api/pets?status=available"
check "Valid status + limit passes"          200 "$BASE/api/pets?status=sold&limit=10"
check "Invalid enum value → 400"            400 "$BASE/api/pets?status=lost"
check "limit above maximum → 400"           400 "$BASE/api/pets?status=sold&limit=500"
check "Non-integer limit → 400"             400 "$BASE/api/pets?status=sold&limit=ten"

# ── Content-Type validation ──────────────────────────────────────────
echo ""
echo "Content-Type validation:"
check "POST with text/plain → 415"          415 -X POST -H "Content-Type: text/plain" -d 'hello' "$BASE/api/pets"
check "POST without Content-Type → 415"     415 -X POST -H "Content-Type:" -d '{}' "$BASE/api/pets"

# ── Body validation ──────────────────────────────────────────────────
echo ""
echo "Body validation:"
check "POST valid body passes"               200 -X POST -H "$JSON" -d '{"name":"Rex","species":"dog"}' "$BASE/api/pets"
check "POST valid body, mixed-case media type passes" 200 -X POST -H "Content-Type: Application/JSON; charset=utf-8" -d '{"name":"Rex","species":"dog"}' "$BASE/api/pets"
check "POST missing required field → 400"   400 -X POST -H "$JSON" -d '{"name":"Rex"}' "$BASE/api/pets"
check "POST invalid enum → 400"             400 -X POST -H "$JSON" -d '{"name":"Rex","species":"fish"}' "$BASE/api/pets"
check "POST malformed JSON → 400"           400 -X POST -H "$JSON" -d '{not json' "$BASE/api/pets"
check "POST empty body when required → 400" 400 -X POST -H "$JSON" "$BASE/api/pets"
check "PUT valid body passes"                200 -X PUT -H "$JSON" -d '{"name":"Rex","status":"sold"}' "$BASE/api/pets/42"
check "PUT invalid field type → 400"        400 -X PUT -H "$JSON" -d '{"name":123}' "$BASE/api/pets/42"

# Larger than client_body_buffer_size on most defaults, forcing nginx to spool
# the body to a temp file; the module must still read it.
BIG_BODY=$(python3 -c 'import json; print(json.dumps({"name": "R" * 40000, "species": "dog"}))')
check "POST large body (temp file) passes"   200 -X POST -H "$JSON" -d "$BIG_BODY" "$BASE/api/pets"

# ── Unvalidated location ─────────────────────────────────────────────
echo ""
echo "Unvalidated location:"
check "Anything goes on /"                   200 "$BASE/whatever"

# ── Custom variables in access log ────────────────────────────────────
echo ""
echo "Custom variables (\$oav_status, \$oav_error_count, \$oav_first_error):"

curl -s -o /dev/null "$BASE/api/pets?status=available"
curl -s -o /dev/null "$BASE/api/pets"
sleep 0.5

if [ -n "${OAV_LOG_CMD:-}" ]; then
    LOGS=$(cd "$SCRIPT_DIR" && eval "$OAV_LOG_CMD")
    check_log "\$oav_status=valid appears in access log"     'oav_status=valid'
    check_log "\$oav_status=invalid appears in access log"   'oav_status=invalid'
    check_log "\$oav_error_count > 0 for invalid request"    'oav_errors=[1-9]'
    check_log "\$oav_first_error contains error detail"      'oav_detail="query\.'
else
    echo "  (skipped: OAV_LOG_CMD not set)"
fi

# ── Results ──────────────────────────────────────────────────────────
echo ""
echo "─────────────────────────────────"
total=$((pass + fail))
echo -e "Results: ${GREEN}$pass passed${NC}, ${RED}$fail failed${NC} out of $total"

if [ "$fail" -gt 0 ]; then
    if [ -n "${OAV_LOG_CMD:-}" ]; then
        echo ""
        echo "Nginx logs:"
        (cd "$SCRIPT_DIR" && eval "$OAV_LOG_CMD") | tail -30
    fi
    exit 1
fi
