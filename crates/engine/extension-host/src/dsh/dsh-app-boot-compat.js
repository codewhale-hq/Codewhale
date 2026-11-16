import compatibility from './compatibility.json' with { type: 'json' }

export function getDshRuntimeVersion() {
  return compatibility.target_dsh_version
}
