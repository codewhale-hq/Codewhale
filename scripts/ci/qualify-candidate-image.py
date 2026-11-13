import json
import os
from pathlib import Path
import shlex
import subprocess


def run(*args):
    print('+ ' + shlex.join(args), flush=True)
    subprocess.run(args, check=True)


source = Path('candidate').resolve()
arch = os.environ['IMAGE_ARCH']
kind = os.environ['IMAGE_KIND']
sha = os.environ['CANDIDATE_SHA']
platform = 'linux/' + arch
machine = {'amd64': 'x86_64', 'arm64': 'aarch64'}[arch]
tag = 'candidate-probe:' + kind
build = ['docker', 'buildx', 'build', '--platform', platform, '--load', '--progress=plain', '--tag', tag]
if kind == 'release':
    run(*build, '--target', 'builder', '--build-arg', 'CODEWHALE_BUILD_SHA=' + sha, str(source))
    run('docker', 'run', '--rm', '--platform', platform, tag, '/bin/sh', '-ec',
        "rustc --version; rustc --version | grep '^rustc 1.100.0 '; rustup toolchain list | tee /tmp/toolchains; test \"$(wc -l < /tmp/toolchains)\" -eq 1; grep '^1.100.0-' /tmp/toolchains")
    run(*build, '--build-arg', 'CODEWHALE_BUILD_SHA=' + sha, str(source))
    run('docker', 'image', 'inspect', tag)
    run('docker', 'run', '--rm', '--platform', platform, '--entrypoint', '/bin/sh', tag, '-ec',
        f'test "$(uname -m)" = {machine}; test "$(id -un)" = codewhale; codewhale --version | grep -F 0.10.3; codew --version | grep -F 0.10.3; cmp /usr/local/bin/codewhale /usr/local/bin/codew')
else:
    config = json.loads((source / '.devcontainer/devcontainer.json').read_text())
    recipe = source / '.devcontainer' / config['build']['dockerfile']
    context = source / '.devcontainer' / config['build']['context']
    for key, value in config['build'].get('args', {}).items():
        build.extend(['--build-arg', key + '=' + value])
    run(*build, '--file', str(recipe), str(context))
    run('docker', 'image', 'inspect', tag)
    command = ['docker', 'run', '--rm', '--platform', platform, '--user', config['remoteUser'],
               '--mount', f'type=bind,source={source},target=/workspaces/CodeWhale,readonly',
               '--workdir', '/workspaces/CodeWhale']
    for mount in config['mounts']:
        command.extend(['--mount', mount])
    for key, value in config['remoteEnv'].items():
        if value.startswith('${localEnv:'):
            continue
        command.extend(['--env', key + '=' + value])
    post_create = config['postCreateCommand']
    assert post_create == 'cargo build --locked', post_create
    probe = f'''set -eu
id
uname -m
test "$(id -un)" = {shlex.quote(config['remoteUser'])}
test "$(uname -m)" = {machine}
rustup toolchain list | tee /tmp/toolchains-before
test "$(wc -l < /tmp/toolchains-before)" -eq 1
grep '^1.100.0-' /tmp/toolchains-before
rustc --version
rustc --version | grep '^rustc 1.100.0 '
cargo --version
rustup show active-toolchain
{post_create}
rustup toolchain list | tee /tmp/toolchains-after
cmp /tmp/toolchains-before /tmp/toolchains-after
'''
    run(*command, tag, '/bin/bash', '-lc', probe)
print('PASS: ' + kind + ' ' + platform + ' candidate=' + sha, flush=True)
