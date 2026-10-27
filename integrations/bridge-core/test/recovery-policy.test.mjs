import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import vm from 'node:vm';
import {ThreadStore} from '../src/lib.mjs';

// Execute the real startup/admission functions with local storage and inert
// runtime/delivery seams. No SDK boot, credentials, network, or model calls.
function extract(source,name) {
  const start=source.search(new RegExp(`^(?:async )?function ${name}\\(`,'m'));
  assert.notEqual(start,-1,`missing ${name}`);
  const rest=source.slice(start),end=rest.slice(1).search(/\n(?:async )?function \w+\(/);
  return end<0?rest:rest.slice(0,end+1);
}
for(const platform of ['telegram','feishu']) {
  const lib=await import(`../../${platform}-bridge/src/lib.mjs`);
  const source=await fs.readFile(new URL(`../../${platform}-bridge/src/index.mjs`,import.meta.url),'utf8');
  const direct=platform==='telegram'?'private':'p2p';
  const baseIdentity={chatId:'chat',chatType:direct,userId:'operator',username:'@operator',openId:'open-operator',unionId:'union-operator',isBot:false};
  async function fixture(t,identity=baseIdentity,policy={}) {
    const dir=await fs.mkdtemp(path.join(os.tmpdir(),'bridge-recovery-'));t.after(()=>fs.rm(dir,{recursive:true,force:true}));
    const file=path.join(dir,'thread-map.json');
    const writer=await ThreadStore.open(file,{messageLimit:200});
    await writer.setChat('chat',{threadId:'thread',activeTurnId:'turn',lastSeq:7,authorizedIdentity:identity,replyToMessageId:'original'});
    const threadStore=await ThreadStore.open(file,{messageLimit:200});
    const calls={runtime:[],sent:[],stream:[],commands:[]};
    const context=vm.createContext({...lib,threadStore,botAccountId:'fixture-bot',config:{allowlist:['operator'],allowGroups:false,allowUnlisted:false,requirePrefixInGroup:false,groupPrefix:'/cw',...policy},
      runtimeJson:async route=>{calls.runtime.push(route);return {turns:[{id:'turn',status:'in_progress'}]};},
      sendText:async(...args)=>calls.sent.push(args),sendTurnText:async(...args)=>calls.sent.push(args),
      streamTurnEvents:async(...args)=>calls.stream.push(args),startTrackedTurnStream:(...args)=>calls.stream.push(args),
      handleCommand:async(...args)=>calls.commands.push(args),answerCallback:async()=>{},callbackAction:()=>({kind:'status'}),handleModalAction:async(...args)=>calls.commands.push(args),
    });
    const names=platform==='telegram'?['recoveryAllowed','reattachActiveTurns','handleIncomingUpdate','handleCallbackQuery','rememberAuthorizedIdentity']:['reattachActiveTurns','handleIncomingMessage'];
    for(const name of names) {
      if(name==='rememberAuthorizedIdentity' && !source.includes('function rememberAuthorizedIdentity(')) continue;
      vm.runInContext(extract(source,name),context);
    }
    return {context,calls,threadStore};
  }
  test(`${platform}: restart rechecks current sender, group policy and saved provenance before any runtime read`,async t=>{
    for(const [identity,policy] of [
      [baseIdentity,{allowlist:['someone-else']}],
      [{...baseIdentity,chatType:'group'},{}],
      [null,{allowlist:['chat']}],
      [null,{allowUnlisted:true}],
      [{...baseIdentity,chatId:'other'},{}],
      [{...baseIdentity,chatType:''},{}],
      ...(platform==='telegram'?[[{...baseIdentity,isBot:true},{}]]:[]),
    ]) {
      const f=await fixture(t,identity,policy);await f.context.reattachActiveTurns();
      assert.deepEqual(f.calls,{runtime:[],sent:[],stream:[],commands:[]});
    }
    for(const policy of [{allowlist:['operator']},{allowlist:['chat']},{allowUnlisted:true},{allowGroups:true}]) {
      const identity=policy.allowGroups?{...baseIdentity,chatType:'group'}:baseIdentity;
      const f=await fixture(t,identity,policy);await f.context.reattachActiveTurns();
      assert.deepEqual(f.calls.runtime,platform==='telegram'?[]:['/v1/threads/thread']);assert.equal(f.calls.sent.length,platform==='telegram'?0:1);assert.equal(f.calls.stream.length,1);
    }
  });
  function incoming(userId,chatType=direct) {
    return platform==='telegram'
      ?{message:{message_id:1,chat:{id:'chat',type:chatType},from:{id:userId,username:'operator'},text:'/status'}}
      :{sender:{sender_id:{user_id:userId}},message:{chat_id:'chat',chat_type:chatType,message_id:'new-reply',message_type:'text',content:JSON.stringify({text:'/status'})}};
  }
  test(`${platform}: only admitted messages can update recovery and reply provenance`,async t=>{
    const f=await fixture(t);
    const handle=platform==='telegram'?f.context.handleIncomingUpdate:f.context.handleIncomingMessage;
    await handle(incoming('revoked'));
    let state=await f.threadStore.getChat('chat');
    assert.equal(state.authorizedIdentity.userId,'operator');assert.equal(state.replyToMessageId,'original');assert.equal(f.calls.commands.length,0);
    // Use a new ID: the first event was recorded as handled, as in production.
    const allowed=incoming('operator');if(platform==='telegram')allowed.message.message_id=2;else allowed.message.message_id='allowed-reply';
    await handle(allowed);state=await f.threadStore.getChat('chat');
    assert.equal(state.authorizedIdentity.chatId,'chat');assert.equal(state.authorizedIdentity.userId,'operator');assert.equal(f.calls.commands.length,1);
    assert.equal(lib.preservedChatStateFields(state).authorizedIdentity,state.authorizedIdentity,'thread replacement must retain authorization provenance');
    assert.equal(Object.hasOwn(state.authorizedIdentity,'text'),false);
    if(platform==='feishu') assert.equal(state.replyToMessageId,'allowed-reply');
  });
  if(platform==='telegram') test('Telegram callbacks persist admitted identity for later recovery',async t=>{
    const f=await fixture(t,null);
    await f.context.handleCallbackQuery({id:'callback',data:'status',message:{chat:{id:'chat',type:'private'},message_id:1},from:{id:'operator'}});
    assert.equal((await f.threadStore.getChat('chat')).authorizedIdentity.userId,'operator');assert.equal(f.calls.commands.length,1);
  });
}
