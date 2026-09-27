import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequestPool } from './requests.js';

test('one cancelled consumer does not abort the shared request', async () => {
  let calls=0, release, network;
  const get=createRequestPool((url,signal)=>{ calls++; network=signal; return new Promise(resolve=>{release=resolve;}); });
  const a=new AbortController(), b=new AbortController();
  const first=get('/resets',a.signal), second=get('/resets',b.signal);
  await Promise.resolve();
  a.abort(); await assert.rejects(first,{name:'AbortError'});
  assert.equal(network.aborted,false); assert.equal(calls,1);
  release({ok:true}); assert.deepEqual(await second,{ok:true});
});

test('all cancelled consumers abandon the request; a retry starts fresh', async () => {
  const signals=[];
  const get=createRequestPool((url,signal)=>{signals.push(signal);return new Promise(()=>{});});
  const c=new AbortController(); const first=get('/resets',c.signal);
  await Promise.resolve(); c.abort(); await assert.rejects(first,{name:'AbortError'});
  assert.equal(signals[0].aborted,true);
  void get('/resets'); await Promise.resolve(); assert.equal(signals.length,2);
});
