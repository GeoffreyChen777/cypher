#!/usr/bin/env bash
set -euo pipefail
test "$#" = 0 || { echo 'No deployment overrides accepted' >&2; exit 2; }
cd "$(dirname "$0")/../.."
: "${CLOUDFLARE_API_TOKEN:?Set the deployment credential in the environment}"
python3 - <<'PY'
import json
from pathlib import Path
c = json.loads(Path('edge/wrangler.development.jsonc').read_text())
assert c['name'] == 'cypher-edge-development'
assert c['account_id'] == '1489e726fc5baa8ecc7a6e1a8c8ed3f8'
assert c['main'] == 'src/development.ts'
assert not c.get('routes') and not c.get('services')
assert c['preview_urls'] is False
assert c['vars']['AUTH_MODE'] == 'dev-locked'
assert c['vars']['NOTIFICATIONS_ENABLED'] == 'false'
assert 'DEV_ACCESS_TOKEN' not in c['vars']
assert {b['binding']: b['bucket_name'] for b in c['r2_buckets']} == {
    'BLOBS': 'cypher-development-blobs', 'RELEASES': 'cypher-development-releases'}
assert all('script_name' not in b for b in c['durable_objects']['bindings'])
assert c['observability']['logs']['invocation_logs'] is False
assert c['observability']['traces']['enabled'] is False
print('Development-only resource boundary verified')
PY
npm --prefix edge run typecheck
npm --prefix edge test
node edge/node_modules/wrangler/bin/wrangler.js deploy --config edge/wrangler.development.jsonc
