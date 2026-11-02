function replaceExactlyOnce(source, pattern, replacement, label) {
  const matches = [...source.matchAll(pattern)]
  if (matches.length !== 1) {
    throw new Error(`Expected one ${label} in the MCP SDK bundle, found ${matches.length}`)
  }
  return source.replace(pattern, replacement)
}

export function redactMcpOAuthLogs(bundle) {
  let output = replaceExactlyOnce(
    bundle,
    /const errorMessage = `\$\{statusCode \? `HTTP \$\{statusCode\}: ` : ""\}Invalid OAuth error response: \$\{error2\}\. Raw body: \$\{body\}`;/g,
    'const errorMessage = `${statusCode ? `HTTP ${statusCode}: ` : ""}OAuth error response details omitted`;',
    'raw OAuth response body interpolation',
  )
  output = replaceExactlyOnce(
    output,
    /console\.warn\(`\[mcp-sdk\] OAuth \$\{JSON\.stringify\(error2\.code\)\} — \$\{action\}\. Cause: \$\{JSON\.stringify\(error2\.message\)\}`\);/g,
    'console.warn(`[mcp-sdk] OAuth authorization failed — ${action}; details omitted.`);',
    'OAuth error detail log',
  )
  return replaceExactlyOnce(
    output,
    /console\.warn\(`\[mcp-sdk\] Could not refresh OAuth tokens; falling back to a new authorization request\. Cause: \$\{JSON\.stringify\(error2 instanceof Error \? error2\.message : String\(error2\)\)\}`\);/g,
    'console.warn("[mcp-sdk] Could not refresh OAuth tokens; falling back to a new authorization request.");',
    'refresh-token error detail log',
  )
}
