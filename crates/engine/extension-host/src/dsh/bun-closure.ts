/** Syntax preparation for Bun runtime imports; parsing never evaluates plugin code. */
import { parse } from 'acorn'
import { pathToFileURL } from 'node:url'

export const REVIEWED_IMPORT = Symbol.for('codewhale.extension-host.reviewed-import')

/** Keep module identity and import.meta source paths; guard every syntactic edge. */
export function prepareBunSource(source: string, path: string, checked: (specifier: string, require: boolean) => string): string {
  const tree = parse(source, { ecmaVersion: 'latest', sourceType: path.endsWith('.cjs') ? 'commonjs' : 'module' })
  const pending: object[] = [tree]
  const edits: { start: number; end: number; source: string }[] = []
  let nodes = 0
  while (pending.length) {
    const node = pending.pop() as Record<string, any>
    if (++nodes > 500_000) throw new Error('reviewed module syntax exceeds the preparation bound')
    if (['ImportDeclaration', 'ExportNamedDeclaration', 'ExportAllDeclaration'].includes(node.type) && node.source) {
      const target = checked(node.source.value, false)
      edits.push({start:node.source.start,end:node.source.end,source:JSON.stringify(target)})
    }
    if (node.type === 'ImportExpression') {
      // The source expression remains unevaluated until the original call site.
      // The host helper then checks its exact receipt, including computed edges.
      edits.push({start:node.start,end:node.start+6,source:`globalThis[Symbol.for(${JSON.stringify(REVIEWED_IMPORT.description)})]`})
      edits.push({start:node.source.end,end:node.source.end,source:`,${JSON.stringify(pathToFileURL(path).href)}`})
    }
    if (node.type === 'CallExpression' && node.callee.type === 'Identifier' && node.callee.name === 'require') {
      if (node.arguments.length !== 1 || node.arguments[0].type !== 'Literal' || typeof node.arguments[0].value !== 'string') {
        throw new Error('computed composition require calls require the diagnosed Node resolver')
      }
      const argument=node.arguments[0]
      edits.push({start:argument.start,end:argument.end,source:JSON.stringify(checked(argument.value,true))})
    }
    if (node.type === 'MemberExpression' && !node.computed
      && ((node.object.type==='MetaProperty' && node.property.name==='require')
        || (node.object.type==='Identifier' && node.object.name==='require' && node.property.name==='resolve'))) {
      throw new Error('runtime composition require resolution requires the diagnosed Node resolver')
    }
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) {
        for (const child of value) if (child && typeof child === 'object' && typeof child.type === 'string') pending.push(child)
      } else if (value && typeof value === 'object' && typeof value.type === 'string') pending.push(value)
    }
  }
  for (const edit of edits.sort((a,b)=>b.start-a.start || b.end-a.end)) source=source.slice(0,edit.start)+edit.source+source.slice(edit.end)
  return source
}
