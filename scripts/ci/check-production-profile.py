"""No cloud development auth in official artifacts/configuration."""
import pathlib
import subprocess
import os
import sys
import plistlib

root = pathlib.Path(__file__).resolve().parents[2]
scheme = (root / 'apps/ios/Cypher.xcodeproj/xcshareddata/xcschemes/Cypher.xcscheme').read_text()
assert 'buildConfiguration = "Release"' in scheme
assert 'Development' not in scheme
for script in ['scripts/package-linux.sh', 'scripts/package-macos.sh']:
    text = (root / script).read_text()
    assert '--features development' not in text
assert 'cypher-edge-development' not in (root / 'edge/wrangler.jsonc').read_text()
if len(sys.argv) > 1:
    artifact = pathlib.Path(sys.argv[1])
    if artifact.is_dir():
        with (artifact / 'Info.plist').open('rb') as f:
            info = plistlib.load(f)
        assert info['CFBundleIdentifier'] == 'ai.mvp-lab.cypher.ios'
        assert info['CypherBuildEnvironment'] == 'production'
        assert info['CFBundleURLTypes'][0]['CFBundleURLSchemes'] == ['cypher']
        assert b'ios-dev-interop-' not in (artifact / info['CFBundleExecutable']).read_bytes()
    else:
        env = dict(os.environ, CYPHER_PROFILE='development')
        result = subprocess.run([str(artifact), 'status'], env=env, text=True, capture_output=True)
        assert result.returncode != 0
        assert 'does not support development Edge authentication' in result.stderr, 'Development-enabled binary cannot be published'
print('Production profile boundary verified')
