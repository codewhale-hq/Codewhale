/** Pure proposals from Core-captured web facts. No HTTP, file, credential, clock or Engine API. */
import type { Json } from '../../protocol.ts'
type Row = Record<string, unknown>
type Entry = { title: string, url: string, snippet?: string }
const own = (value: Row, key: string) => Object.prototype.hasOwnProperty.call(value, key)
function row(value: unknown): Row { if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid captured web record'); return value as Row }
function text(value: unknown): string { if (typeof value !== 'string') throw new Error('invalid captured web text'); return value }
function string(value: unknown): string | undefined { return typeof value === 'string' ? value : undefined }
function array(value: unknown): unknown[] { return Array.isArray(value) ? value : [] }
function count(value: unknown, max = 10): number { if (!Number.isSafeInteger(value) || (value as number) < 0 || (value as number) > max) throw new Error('invalid captured web count'); return value as number }
function rustTrim(value: string): string { return value.replace(/^[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+|[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+$/g, '') }
function firstPresent(value: Row, keys: string[]): unknown { for (const key of keys) if (own(value,key)) return value[key]; return undefined }
function firstText(value: Row, keys: string[]): string | undefined { for (const key of keys) { const found = string(value[key]); if (found !== undefined) { const trimmed = rustTrim(found); if (trimmed) return trimmed } } return undefined }
function integer(facts: Row, path: string): string | undefined { const value = facts[path]; if (value === undefined) return undefined; const number = row(value); if (number.i64 === null || number.i64 === undefined) return undefined; const result = text(number.i64); if (!/^-?\d{1,19}$/.test(result) || BigInt(result) < -9223372036854775808n || BigInt(result)>9223372036854775807n) throw new Error('invalid captured web integer'); return result }
function number(facts: Row, path: string): number { const value = facts[path]; if (value === undefined) return 0; const result = Number(text(row(value).score)); if (!Number.isFinite(result)) throw new Error('invalid captured web score'); return result }
function result(metadata: Json) { return { ok: true as const, result: { success: true, content: '', metadata } } }

function filters(input: Row): Json {
  const recency = input.recency_days === null || input.recency_days === undefined ? undefined : count(input.recency_days,3650)
  const locale = input.locale === null || input.locale === undefined ? undefined : rustTrim(text(input.locale))
  const parts = locale?.split(/[-_]/)
  const language = parts?.[0] !== undefined && /^[A-Za-z]{2,3}$/.test(parts[0]) ? parts[0].toLowerCase() : null
  const region = parts?.[1] !== undefined && /^[A-Za-z]{2}$/.test(parts[1]) ? parts[1].toUpperCase() : null
  const window = recency === undefined ? null : recency <= 1 ? 'day' : recency <= 7 ? 'week' : recency <= 31 ? 'month' : 'year'
  return { kind:'web_filters', window, language, region }
}

function validJson(value: unknown): boolean {
  const pending:{value:unknown,depth:number}[]=[{value,depth:0}]
  while(pending.length) {
    const item=pending.pop()!
    if(typeof item.value==='number'&&!Number.isFinite(item.value))return false
    if(typeof item.value==='string'&&[...item.value].some(ch=>ch.length===1&&ch.charCodeAt(0)>=0xd800&&ch.charCodeAt(0)<=0xdfff))return false
    if(item.value&&typeof item.value==='object') {
      if(item.depth>=128)return false
      for(const child of Object.values(item.value))pending.push({value:child,depth:item.depth+1})
    }
  }
  return true
}

function provider(input: Row): Json {
  const backend = text(input.backend), limit = count(input.max_results), parsed = row(input.parsed), facts = row(input.number_facts)
  let entries: Entry[] = [], error: string | null = null
  let items: unknown[] = [], titleKeys = ['title'], urlKeys = ['url'], snippetKeys = ['content','snippet'], trim = true, capSnippet = false, scorePath: string | undefined
  switch (backend) {
    case 'tavily': items = array(parsed.results); break
    case 'firecrawl': {
      const data = parsed.data
      items = array(data && typeof data === 'object' && !Array.isArray(data) && own(row(data),'web') ? row(data).web : data)
      snippetKeys=['description','markdown','content']; capSnippet=true
      if (parsed.success === false) error=`Firecrawl search failed: ${firstText(parsed,['error','message']) ?? 'unknown API error'}`
      break
    }
    case 'metaso': {
      items=array(parsed.webpages); urlKeys=['link']; snippetKeys=['snippet','summary']
      const code=integer(facts,'/code')
      if (code !== undefined && code !== '0') error = code === '3003' ? 'Metaso: daily search limit reached — set METASO_API_KEY or get one at https://metaso.cn/search-api/playground' : code === '2005' ? 'Metaso API key rejected — check METASO_API_KEY or set `[search] api_key` in config.toml' : `Metaso API error (code ${code}: ${string(parsed.message) ?? 'unknown error'})`
      break
    }
    case 'bocha': {
      const data = parsed.data && typeof parsed.data === 'object' && !Array.isArray(parsed.data) ? row(parsed.data) : undefined
      let pages: unknown
      if (data) {
        const web = data.webPages && typeof data.webPages === 'object' && !Array.isArray(data.webPages) ? row(data.webPages) : undefined
        pages = web && own(web,'value') ? web.value : data.pages
      }
      if (pages === undefined) pages=parsed.pages
      items=array(pages); titleKeys=['name','title']; urlKeys=['url','link']; snippetKeys=['summary','snippet','description']
      const code=integer(facts,'/code')
      if (code !== undefined && code !== '0' && code !== '200') error=`Bocha search API error (code ${code}: ${string(firstPresent(parsed,['msg','message'])) ?? 'unknown error'})`
      break
    }
    case 'baidu': {
      items=array(parsed.references); titleKeys=['title','name']; urlKeys=['url','link']; snippetKeys=['content','snippet','summary']
      const key=own(parsed,'error_code')?'error_code':'code', code=integer(facts,`/${key}`)
      if (code !== undefined && code !== '0') error=`Baidu search API error (code ${code}: ${string(firstPresent(parsed,['error_msg','message'])) ?? 'unknown error'})`
      break
    }
    case 'searxng': items=array(parsed.results); scorePath='/results'; break
    case 'sofya': items=array(parsed.results); snippetKeys=['content','description']; trim=false; break
    case 'serply': items=array(parsed.results); urlKeys=['link']; snippetKeys=['description','snippet']; trim=false; break
    case 'volcengine': {
      if (own(parsed,'error')) {
        const value = parsed.error && typeof parsed.error==='object' && !Array.isArray(parsed.error) ? row(parsed.error) : {}
        error=`Volcengine API error (code ${string(value.code) ?? 'unknown'}: ${string(value.message) ?? 'no details'})`
      }
      let response: string | undefined
      const message=array(parsed.output).slice().reverse().find(value=>value && typeof value==='object' && !Array.isArray(value) && row(value).type==='message')
      if (message) { const content=array(row(message).content).find(value=>value && typeof value==='object' && !Array.isArray(value) && typeof row(value).text==='string'); if(content) response=text(row(content).text) }
      if (response === undefined && error === null) error='Volcengine response contains no output text'
      if (response !== undefined) {
        let body=response
        const fence=response.indexOf('```json')
        if(fence>=0) {const rest=response.slice(fence+7),end=rest.indexOf('```');if(end>=0)body=rustTrim(rest.slice(0,end));else {const first=response.indexOf('{'),last=response.lastIndexOf('}');if(first>=0&&last>=first)body=response.slice(first,last+1)}}
        else {const first=response.indexOf('{'),last=response.lastIndexOf('}');if(first>=0&&last>=first)body=response.slice(first,last+1)}
        try {const value=JSON.parse(body);if(!validJson(value))throw new Error('model JSON failed Core-compatible scalar/depth guard');if(value&&typeof value==='object'&&!Array.isArray(value))items=array(row(value).results)} catch {items=[]}
      }
      snippetKeys=['snippet']; break
    }
    default: throw new Error('unadmitted web provider')
  }
  const scored: {entry:Entry,score:number}[]=[]
  for (const [index,value] of items.entries()) {
    if(!value || typeof value!=='object' || Array.isArray(value))continue
    const item=row(value),rawTitle=string(firstPresent(item,titleKeys)),rawUrl=string(firstPresent(item,urlKeys))
    if(rawTitle===undefined||rawUrl===undefined)continue
    const title=trim?rustTrim(rawTitle):rawTitle,url=trim?rustTrim(rawUrl):rawUrl
    if(trim&&(!title||!url))continue
    // Some legacy providers select the first present snippet alias even if it
    // is null/wrong/empty; others choose the first nonempty string.
    const presentSnippet=['bocha','baidu','volcengine'].includes(backend)
    const selected=presentSnippet?string(firstPresent(item,snippetKeys)):undefined
    const snippet=presentSnippet?(selected===undefined?undefined:rustTrim(selected)||undefined):firstText(item,snippetKeys)
    const entry={title,url,...(snippet===undefined?{}:{snippet:capSnippet?[...snippet].slice(0,1000).join(''):snippet})}
    scored.push({entry,score:scorePath===undefined?0:number(facts,`${scorePath}/${index}/score`)})
    if(scorePath===undefined&&scored.length===limit)break
  }
  if(scorePath!==undefined)scored.sort((a,b)=>a.score === b.score ? Object.is(a.score,-0) === Object.is(b.score,-0) ? 0 : Object.is(a.score,-0) ? 1 : -1 : a.score > b.score ? -1 : 1)
  entries=scored.slice(0,limit).map(value=>value.entry)
  return {kind:'web_provider',entries,error}
}

function extraction(input: Row): Json {
  if(!Array.isArray(input.candidates) || input.candidates.length>3)throw new Error('invalid captured web regions')
  const facts: {id:number,chars:number,words:number}[]=[]
  let previous=-1
  for(const value of input.candidates) {
    const candidate=row(value),id=count(candidate.id,2)
    if(Object.keys(candidate).some(key=>!['id','non_whitespace','words'].includes(key)) || id<=previous)throw new Error('invalid ordered captured web region');previous=id
    facts.push({id,chars:count(candidate.non_whitespace,100_000_000),words:count(candidate.words,100_000_000)})
  }
  return {kind:'web_extract',candidate:facts.find(value=>value.chars>=32&&value.words>=5)?.id ?? null}
}

function images(input: Row): Json {
  const limit=count(input.max_results),parsed=row(input.parsed)
  if(!Array.isArray(parsed.results))throw new Error('invalid captured image results')
  const entries:Json[]=[]
  for(const value of parsed.results) {
    const entry=row(value),image=text(entry.image)
    for(const key of ['thumbnail','title','url','source'])if(entry[key]!==null&&entry[key]!==undefined)text(entry[key])
    for(const key of ['width','height'])if(entry[key]!==null&&entry[key]!==undefined)count(entry[key],4294967295)
    if(!rustTrim(image))continue
    entries.push({image,...Object.fromEntries(['thumbnail','title','url','source','width','height'].filter(key=>entry[key]!==null&&entry[key]!==undefined).map(key=>[key,entry[key] as Json]))})
    // Domain filtering is mandatory Core URL policy and happens before the
    // final count cap, so all bounded candidates are preserved here.
  }
  return {kind:'web_images',entries,max_results:limit}
}

// Core chooses the endpoint, method, model and credential injection. This
// adapter owns only the keyless query payload or query-pair proposal.
function request(input: Row): Json {
  const backend=text(input.backend), query=text(input.query), max=count(input.max_results)
  const f=row(input.filters), window=f.window===null?undefined:text(f.window), region=f.region===null?undefined:text(f.region), language=f.language===null?undefined:text(f.language)
  const locale=input.locale===null||input.locale===undefined?undefined:text(input.locale)
  let payload:Json=null, pairs:Json=[]
  switch(backend) {
    case 'firecrawl': payload={query,limit:max,sources:[{type:'web'}],...(window===undefined?{}:{tbs:`qdr:${window[0]}`}),...(region===undefined?{}:{country:region})};break
    case 'tavily':payload={query,search_depth:'basic',max_results:max,...(window===undefined?{}:{time_range:window})};break
    case 'bocha':payload={query,freshness:'noLimit',count:max};break
    case 'metaso':payload={q:query,scope:'webpage',size:Math.max(1,Math.min(100,max))};break
    case 'sofya':payload={query,max_results:max};break
    case 'baidu':payload={messages:[{role:'user',content:query}],search_source:'baidu_search_v2',resource_type_filter:[{type:'web',top_k:max}]};break
    case 'volcengine':payload={input:[{role:'user',content:[{type:'input_text',text:`Search the web for: ${query}\n\nCRITICAL: Respond ONLY with a valid JSON object. No markdown, no explanation.\nSchema: {"results":[{"title":"...","url":"https://...","snippet":"..."}]}\n- results: 1-${max} most relevant pages\n- title: page title (required)\n- url: full URL starting with https:// (required)\n- snippet: 1-2 sentence factual summary (required)\n- If zero results: {"results":[]}\n- Your entire response must be valid, parseable JSON.`}]}]};break
    case 'serply':pairs=[['q',query],['num',String(max)],...(language===undefined?[]:[['hl',language]]),...(region===undefined?[]:[['gl',region.toLowerCase()]])];break
    case 'searxng':pairs=[['q',query],['format','json'],...(window===undefined?[]:[['time_range',window]]),...(locale===undefined?[]:[['language',locale]])];break
    case 'bing':case 'duckduckgo':pairs=[['q',query]];break
    default:throw new Error('unadmitted web request provider')
  }
  return {kind:'web_request',payload,pairs}
}

// These are already-parsed native/scraper candidates. URLs are opaque Core
// handles; no credentials, configured endpoint, source ref or domain crosses.
function entries(input: Row): Json {
  if(!Array.isArray(input.entries))throw new Error('invalid captured web candidates')
  const values:Json[]=input.entries.map(value=>{
    const item=row(value)
    return {title:text(item.title),url:text(item.url),...(item.snippet===null||item.snippet===undefined?{}:{snippet:text(item.snippet)}),...(item.published===null||item.published===undefined?{}:{published:text(item.published)})}
  })
  return {kind:'web_entries',entries:values}
}

function finalization(input: Row): Json {
  const requested=row(input.requested),capabilities=row(input.capabilities),counted=count(input.count)
  if(!Array.isArray(input.degraded))throw new Error('invalid captured web receipt')
  const degraded:Json[]=input.degraded.map(value=>row(value) as Json)
  const ignored=(knob:string)=>degraded.some(value=>row(value).kind==='knob_ignored'&&row(value).knob===knob)
  const honored={max_results:capabilities.max_results==='supported',recency:false,domains:requested.domains===true,locale:false}
  for(const knob of ['recency','locale'] as const) {
    if(knob==='locale') {
      if(!Array.isArray(input.domain_extra))throw new Error('invalid captured domain receipt')
      degraded.push(...input.domain_extra.map(value=>row(value) as Json))
    }
    if(requested[knob]===true&&!ignored(knob)) {
      if(capabilities[knob]==='supported')honored[knob]=true
      else degraded.push({kind:'knob_ignored',knob})
    }
  }
  const hasNote=input.has_note===true
  const prefix=counted===0 ? hasNote?'No results found. ':'No results found' : `Found ${counted} result(s)${hasNote?'. ':''}`
  const suffix=degraded.some(value=>row(value).kind==='answer_cut_by_provider')?'\n[the provider stopped the search answer at its output limit; the answer is incomplete]':''
  return {kind:'web_finalize',honored,degraded,prefix,suffix}
}

export function transformWebSnapshot(operation: string, value: unknown) {
  const input=row(value)
  switch(operation) {
    case 'web_request':return result(request(input))
    case 'web_entries':return result(entries(input))
    case 'web_finalize':return result(finalization(input))
    case 'web_filters':return result(filters(input))
    case 'web_provider':return result(provider(input))
    case 'web_extract':return result(extraction(input))
    case 'web_images':return result(images(input))
    default:throw new Error('unadmitted web adapter operation')
  }
}
