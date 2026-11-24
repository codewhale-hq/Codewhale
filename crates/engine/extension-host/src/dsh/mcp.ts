/** Raw DSH MCP rows submit definitions to the existing Rust MCP owner. */
import type { Context } from '@deepseek-ai/cordis'
import { normalizeMcp, type McpDefinition } from '../shims/mcp.ts'
export interface ReviewedMcpContext extends Context { mcp:{registerServer(definition:McpDefinition):()=>void} }
export function reviewedMcpModule() {
  return { name:'codewhale-reviewed-mcp',inject:['mcp'],
    apply(ctx:ReviewedMcpContext,config:Record<string,unknown>) {
      if(!config||Array.isArray(config)||Object.keys(config).some(k=>!['transport','serverName','command','args','env','cwd','url','headers','toolCallTimeoutMs','failOnStartupError'].includes(k)))throw new Error('MCP row uses unsupported configuration; Core owns reconnect and instruction bounds')
      if(typeof config.serverName!=='string')throw new Error('MCP serverName must be literal')
      const options:Record<string,unknown>={}
      if(config.toolCallTimeoutMs!==undefined) { const timeout=config.toolCallTimeoutMs;if(typeof timeout!=='number'||!Number.isInteger(timeout)||timeout<1000||timeout>86_400_000||timeout%1000!==0)throw new Error('MCP timeout must be whole seconds within Core bounds');options.execute_timeout=timeout/1000 }
      if(config.failOnStartupError!==undefined&&typeof config.failOnStartupError!=='boolean')throw new Error('MCP startup gate must be a boolean')
      if(config.failOnStartupError===true)options.required=true
      let server:Record<string,unknown>
      if(config.transport==='stdio') {
        const cwd=config.cwd??''
        if(typeof cwd!=='string'||(cwd!==''&&cwd!=='.'&&(!cwd||/[\\:\u0000-\u001f]/u.test(cwd)||cwd.split('/').some(p=>!p||p==='.'||p==='..'))))throw new Error('MCP cwd must be a reviewed bundle-relative directory')
        server={type:'stdio',command:config.command,args:config.args??[],env:config.env??{},cwd:cwd===''||cwd==='.'?'source':`source/${cwd}`}
      }else if(config.transport==='streamable-http'||config.transport==='sse')server={type:config.transport,url:config.url,headers:config.headers??{}}
      else throw new Error('MCP transport must be stdio, streamable-http or sse')
      if(Object.keys(options).length)server.extensions={'net.codewhale':options}
      ctx.mcp.registerServer(normalizeMcp({serverName:config.serverName,server}))
    },
  }
}
