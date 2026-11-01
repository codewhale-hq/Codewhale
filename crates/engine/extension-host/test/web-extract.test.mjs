import test from 'node:test'
import assert from 'node:assert/strict'
import { transformStockSnapshot } from '../dist/stock-adapters.mjs'
const choose=candidates=>transformStockSnapshot({kind:'stock_adapter',operation:'web_extract',input:{candidates}}).result.metadata
const region=(id,non_whitespace=32,words=5)=>({id,non_whitespace,words})
test('article/main/body choice follows captured legacy order',()=>{assert.equal(choose([region(0),region(1),region(2)]).candidate,0);assert.equal(choose([region(0,31),region(1),region(2)]).candidate,1);assert.equal(choose([region(0,32,4),region(2)]).candidate,2)})
test('threshold uses scalar facts and large complete regions never cross IPC',()=>{assert.deepEqual(choose([region(0,31,100),region(1,999,4)]),{kind:'web_extract',candidate:null});assert.equal(choose([region(0,10_000_000,50_000)]).candidate,0)})
test('every region is validated before choosing, including malformed later and duplicate IDs',()=>{for(const candidates of [[region(0),region(1,-1)],[region(1),region(0)],[region(0),region(0)],[region(3)],[{...region(0),html:'private body'}],[region(0,1.2)]])assert.throws(()=>choose(candidates))})
test('missing regions are not invented and empty capture is the ordinary shell result',()=>{assert.equal(choose([]).candidate,null);assert.equal(choose([region(2)]).candidate,2);assert.equal(choose([region(0,0,0)]).candidate,null)})
