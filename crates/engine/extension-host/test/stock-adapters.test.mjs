import test from 'node:test'
import assert from 'node:assert/strict'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
import { createHarnessModule } from '../dist/builtin/harness.mjs'

const project = (operation, input) => ({ kind: 'stock_adapter', operation, input })
const request = { requested_ticker: 'BTC', resolved_symbol: 'BTC-USD' }
const quote = extra => project('finance_quote', { request, parsed: { quoteResponse: { result: [{ symbol: 'btc-usd', regularMarketPrice: 125, regularMarketPreviousClose: 100, regularMarketTime: '9223372036854775807', ...extra }] } } })
const chart = extra => project('finance_chart', { request, parsed: { chart: { result: [{ meta: { symbol: 'BTC-USD', regularMarketPrice: 125, chartPreviousClose: 100, previousClose: 50, ...extra } }], error: null } } })
const metadata = value => transformStockSnapshot(value).result.metadata
const owner = { plugin_id: 'host:harness', generation: 4, owner_token: 'opaque-fixture-owner' }
const params = extra => ({ owner, execution_id: 'captured-one', ticket: 'opaque-fixture-ticket', deadline_ms: 1000, ...extra })

test('quote normalization preserves priority, changes and exact i64 timestamps', () => {
  assert.deepEqual(metadata(quote({ longName: 'Long', shortName: 'Short', exchange: 'low', fullExchangeName: 'Full' })), {
    requested_ticker: 'BTC', ticker: 'btc-usd', price: '125', previous_close: '100', change: '25', change_percent: '25',
    source: 'yahoo_quote', fallback_used: false, name: 'Long', exchange: 'Full', market_time: '9223372036854775807',
  })
  const result = metadata(quote({ regularMarketChange: 0, regularMarketChangePercent: 0, longName: '', regularMarketTime: '-9223372036854775808' }))
  assert.equal(result.change, '0'); assert.equal(result.change_percent, '0'); assert.equal(result.name, '')
  assert.equal(result.market_time, '-9223372036854775808')
})
test('numeric wire preserves negative zero, EPSILON policy and derived infinities', () => {
  const zero = metadata(quote({ regularMarketPrice: -0, regularMarketPreviousClose: 0 }))
  assert.equal(zero.price, '-0'); assert.equal(zero.change, '-0'); assert.equal('change_percent' in zero, false)
  assert.equal(metadata(quote({ regularMarketPreviousClose: Number.EPSILON })).change_percent, String(((125 - Number.EPSILON) / Number.EPSILON) * 100))
  const overflow = metadata(quote({ regularMarketPrice: 1e308, regularMarketPreviousClose: -1e308 }))
  assert.equal(overflow.change, 'inf'); assert.equal(overflow.change_percent, '-inf')
})
test('chart fallback uses chart close and the existing field priorities', () => {
  assert.deepEqual(metadata(chart({ longName: 'Long', instrumentType: 'CRYPTOCURRENCY', exchangeName: 'low', fullExchangeName: 'Full' })), {
    requested_ticker: 'BTC', ticker: 'BTC-USD', price: '125', previous_close: '100', change: '25', change_percent: '25',
    source: 'yahoo_chart', fallback_used: true, name: 'Long', quote_type: 'CRYPTOCURRENCY', exchange: 'Full',
  })
})
test('upstream not-found and missing-price failures retain exact details', () => {
  const noResult = project('finance_quote', { request, parsed: { quoteResponse: { result: [] } } })
  assert.deepEqual(metadata(noResult), { endpoint: 'yahoo_quote', kind: 'not_found', detail: "no result for symbol 'BTC-USD'" })
  assert.deepEqual(metadata(quote({ regularMarketPrice: null })), { endpoint: 'yahoo_quote', kind: 'upstream', detail: 'response missing regularMarketPrice' })
  for (const error of [{ code: 'Not Found', description: 'missing' }, { description: 'Symbol may be delisted' }]) {
    assert.equal(metadata(project('finance_chart', { request, parsed: { chart: { error } } })).kind, 'not_found')
  }
  assert.equal(metadata(project('finance_chart', { request, parsed: { chart: { error: { code: 'rate limit', description: 'retry' } } } })).kind, 'upstream')
})
test('symbol matching uses Rust ASCII case folding rather than Unicode expansion', () => {
  const value = project('finance_quote', { request: { requested_ticker: 'SS', resolved_symbol: 'SS' }, parsed: { quoteResponse: { result: [{ symbol: 'ß', regularMarketPrice: 1 }] } } })
  assert.equal(metadata(value).kind, 'not_found')
})

const parser = value => ({ ok: true, value })
const error = message => ({ ok: false, error: message })
const data = extra => project('validate_data', { format: 'auto', source: 'inline', extension: null, json: error('JSON diagnostic'), toml: error('TOML diagnostic'), ...extra })
test('data auto selects JSON then TOML, while a file extension pins its parser', () => {
  assert.deepEqual(metadata(data({ json: parser({ kind: 'array', entries: 3 }), toml: parser({ kind: 'table', keys: ['a'] }) })), { valid: true, format: 'json', source: 'inline', summary: { top_level: 'array', entries: 3 } })
  assert.equal(metadata(data({ toml: parser({ kind: 'table', keys: ['a'] }) })).format, 'toml')
  const pinned = transformStockSnapshot(data({ extension: 'json', toml: parser({ kind: 'table', keys: [] }) })).result
  assert.equal(pinned.success, false); assert.equal(pinned.content, 'Invalid JSON: JSON diagnostic')
})
test('data previews preserve Core captured ordering, ten-key cap and primitive types', () => {
  const keys = ['😀', '\ue000', 'z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', '__proto__']
  for (const captured of [keys, keys.slice().sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)))]) {
    const summary = metadata(data({ json: parser({ kind: 'object', keys: captured }) })).summary
    assert.deepEqual(summary.keys_preview, captured.slice(0, 10))
    assert.equal(summary.entries, 12)
  }
  for (const kind of ['string', 'number', 'boolean', 'null']) assert.equal(metadata(data({ json: parser({ kind }) })).summary.top_level, kind)
  for (const kind of ['string', 'integer', 'float', 'boolean', 'datetime']) assert.equal(metadata(data({ format: 'toml', toml: parser({ kind }) })).summary.top_level, kind)
})
test('parser diagnostics remain exact captured bytes and auto errors are unchanged', () => {
  const result = transformStockSnapshot(data({ json: error('line 1:\n  bad 😀'), toml: error('toml\nspan') })).result
  assert.equal(result.content, 'Validation failed in auto mode: content is neither valid JSON nor TOML.')
  assert.deepEqual(result.metadata, { valid: false, format: 'auto', source: 'inline', json_error: 'line 1:\n  bad 😀', toml_error: 'toml\nspan' })
})
test('malformed, unknown and oversized snapshots refuse instead of producing results', () => {
  for (const value of [null, {}, project('fetch', {}), quote({ regularMarketPrice: '125' }), quote({ regularMarketTime: '9223372036854775808' }), data({ json: parser({ kind: 'object', keys: ['a', 'a'] }) }), data({ json: parser({ kind: 'array', entries: -1 }) }), data({ source: 'x'.repeat(1024 * 1024) })]) assert.throws(() => transformStockSnapshot(value))
})

test('actual pinned harness transforms one captured reply and sends only an opaque ticket', async () => {
  const calls = []
  const runner = createHarnessModule({ async request(method, input) { calls.push([method, input]); return quote({}) } }, owner)
  const result = await runner.run(params(), new AbortController().signal)
  assert.equal(result.result.metadata.price, '125')
  assert.deepEqual(calls, [['exec/redeem', { owner, execution_id: 'captured-one', ticket: 'opaque-fixture-ticket' }]])
  await runner.dispose()
})
test('wrong owner cannot redeem a stock operation and malformed reply has no fallback', async () => {
  let calls = 0
  const runner = createHarnessModule({ async request() { calls++; return project('unknown', {}) } }, owner)
  await assert.rejects(runner.run(params({ owner: { ...owner, plugin_id: 'native' } }), new AbortController().signal), /stale/)
  assert.equal(calls, 0)
  await assert.rejects(runner.run(params(), new AbortController().signal), /unadmitted/)
  assert.equal(calls, 1); await runner.dispose()
})
test('stock cancellation and owner disposal settle before a late captured reply', async () => {
  let finish
  const controller = new AbortController()
  const runner = createHarnessModule({ request() { return new Promise(resolve => { finish = resolve }) } }, owner)
  const pending = runner.run(params(), controller.signal)
  controller.abort()
  await assert.rejects(pending, /cancelled/)
  finish(quote({})); await runner.dispose()
  await assert.rejects(runner.run(params(), new AbortController().signal), /stale/)
})
