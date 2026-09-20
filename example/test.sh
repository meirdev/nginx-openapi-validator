#!/usr/bin/env bash
#
# End-to-end test for the nginx OpenAPI validator module using Docker Compose.
#
# Usage:
#   ./example/test.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
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

echo "=== Nginx OpenAPI Validator - Integration Tests ==="
echo ""

# Start services
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

# Wait for nginx to be ready
echo "Waiting for nginx..."
for i in $(seq 1 10); do
    if curl -s -o /dev/null http://localhost:$PORT/ 2>/dev/null; then
        break
    fi
    sleep 0.5
done

BASE="http://localhost:$PORT"

# ── Path validation ──────────────────────────────────────────────────
echo ""
echo "Path validation:"
check "Valid path /api/pets passes"          200 "$BASE/api/pets?status=available"
check "Valid path /api/pets/42 passes"       200 "$BASE/api/pets/42"
check "Unknown path /api/unknown → 404"     404 "$BASE/api/unknown"
check "Unknown nested path → 404"           404 "$BASE/api/foo/bar"

# ── Method validation ────────────────────────────────────────────────
echo ""
echo "Method validation:"
check "GET /api/pets is allowed"             200 "$BASE/api/pets?status=available"
check "DELETE /api/pets → 405"              405 -X DELETE "$BASE/api/pets"
check "PATCH /api/pets → 405"               405 -X PATCH "$BASE/api/pets"
check "DELETE /api/pets/42 is allowed"       200 -X DELETE "$BASE/api/pets/42"

# ── Query parameter validation ───────────────────────────────────────
echo ""
echo "Query parameter validation:"
check "Missing required 'status' → 400"    400 "$BASE/api/pets"
check "Valid status=available passes"        200 "$BASE/api/pets?status=available"
check "Valid status + limit passes"          200 "$BASE/api/pets?status=sold&limit=10"

# ── Content-Type validation ──────────────────────────────────────────
# NOTE: POST body validation requires body reading (not yet implemented).
# These tests validate content-type checking only.
echo ""
echo "Content-Type validation:"
check "POST with text/plain → 415"          415 -X POST -H "Content-Type: text/plain" -d 'hello' "$BASE/api/pets"

# POST with correct content-type returns 400 because body reading is not yet
# implemented (body is always None, so "required body missing" triggers).
check "POST with json ct, missing body → 400 (body read not impl)" 400 -X POST -H "Content-Type: application/json" -d '{"name":"Rex","species":"dog"}' "$BASE/api/pets"

# ── Unvalidated location ─────────────────────────────────────────────
echo ""
echo "Unvalidated location:"
check "Anything goes on /"                   200 "$BASE/whatever"

# ── Custom variables in access log ────────────────────────────────────
echo ""
echo "Custom variables (\$oav_status, \$oav_error_count, \$oav_first_error):"

# Send a valid request and an invalid one, then check the access log
curl -s -o /dev/null http://localhost:$PORT/api/pets?status=available
curl -s -o /dev/null http://localhost:$PORT/api/pets
sleep 0.5

LOGS=$(docker compose logs nginx 2>&1)

# Check that oav_status=valid appears for the successful request
if echo "$LOGS" | grep -q 'oav_status=valid'; then
    echo -e "  ${GREEN}✓${NC} \$oav_status=valid appears in access log"
    pass=$((pass + 1))
else
    echo -e "  ${RED}✗${NC} \$oav_status=valid not found in access log"
    fail=$((fail + 1))
fi

# Check that oav_status=invalid appears for the failing request
if echo "$LOGS" | grep -q 'oav_status=invalid'; then
    echo -e "  ${GREEN}✓${NC} \$oav_status=invalid appears in access log"
    pass=$((pass + 1))
else
    echo -e "  ${RED}✗${NC} \$oav_status=invalid not found in access log"
    fail=$((fail + 1))
fi

# Check that oav_errors count is > 0 for invalid request
if echo "$LOGS" | grep -q 'oav_errors=[1-9]'; then
    echo -e "  ${GREEN}✓${NC} \$oav_error_count > 0 for invalid request"
    pass=$((pass + 1))
else
    echo -e "  ${RED}✗${NC} \$oav_error_count not found in access log"
    fail=$((fail + 1))
fi

# Check that oav_detail contains error message for invalid request
if echo "$LOGS" | grep -q 'oav_detail="query\.'; then
    echo -e "  ${GREEN}✓${NC} \$oav_first_error contains error detail"
    pass=$((pass + 1))
else
    echo -e "  ${RED}✗${NC} \$oav_first_error detail not found in access log"
    fail=$((fail + 1))
fi

# ── Results ──────────────────────────────────────────────────────────
echo ""
echo "─────────────────────────────────"
total=$((pass + fail))
echo -e "Results: ${GREEN}$pass passed${NC}, ${RED}$fail failed${NC} out of $total"

if [ "$fail" -gt 0 ]; then
    echo ""
    echo "Nginx logs:"
    docker compose logs nginx 2>&1 | tail -30
    exit 1
fi

echo ""
echo "Sample access log lines:"
docker compose logs nginx 2>&1 | grep "oav_status=" | tail -5
