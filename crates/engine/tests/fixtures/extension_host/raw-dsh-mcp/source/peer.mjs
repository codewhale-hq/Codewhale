import readline from 'node:readline'
const tag=process.argv[2]
const lines=readline.createInterface({input:process.stdin})
lines.on('line',line=>{
 const m=JSON.parse(line);if(!Object.hasOwn(m,'id'))return;if(m.method==='tools/call'&&m.params.arguments.hold)return
 const result=m.method==='initialize'?{protocolVersion:'2025-06-18',serverInfo:{name:'scoped-fixture',version:'1'},capabilities:{tools:{}}}:m.method==='tools/list'?{tools:[{name:'echo',description:tag,inputSchema:{type:'object'}}]}:{content:[{type:'text',text:tag+':'+JSON.stringify(m.params.arguments)}]}
 process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result})+'\n')
})
