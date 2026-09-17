import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { Status } from './types';
export type LineTestResult = { testId:string; lineId:string; revision:string; medianMs:number|null; state:'success'|'failed'|'cancelled'; completedAt:number };
type Saved = LineTestResult & { automatic:boolean; received:number; monotonic:number };
export function useLineTests(status:Status) {
  const [result,setResult]=useState<Saved|null>(null);
  const [pending,setPending]=useState<string|null>(null);
  const active=useRef<string|null>(null);
  const latest=useRef(status);latest.current=status;
  const scope=JSON.stringify([status.settings.mode,status.settings.lineSelection,status.settings.selectedLineId,status.running,status.acceleration.lineId,status.authorization.state,status.authorization.deviceId,status.accelerationLines]);
  function clear(){const id=active.current;active.current=null;setPending(null);setResult(null);if(id)void invoke('cancel_line_test',{testId:id}).catch(()=>{});}
  useEffect(()=>{clear();},[scope]);
  useEffect(()=>{const timer=setInterval(()=>setResult(old=>old&&(Date.now()<old.received||Date.now()-old.received>=300000||performance.now()-old.monotonic>=300000)?null:old),1000);return()=>{clearInterval(timer);const id=active.current;if(id)void invoke('cancel_line_test',{testId:id}).catch(()=>{});};},[]);
  async function test(lineId:string|null,automatic:boolean){
    if(active.current)return;
    const id=crypto.randomUUID();active.current=id;setPending(lineId??'auto:');setResult(null);
    try{
      const value=await invoke<LineTestResult>(lineId===null?'test_auto_lines':'test_line',{testId:id,...(lineId===null?{}:{lineId})});
      if(active.current!==id||value.testId!==id||value.state==='cancelled')return;
      const line=latest.current.accelerationLines.find(l=>l.id===value.lineId);
      if(value.state==='success'&&(!line||(line.revision??'')!==value.revision))return;
      setResult({...value,automatic,received:Date.now(),monotonic:performance.now()});
    }catch{if(active.current===id)setResult({testId:id,lineId:lineId??'',revision:'',medianMs:null,state:'failed',completedAt:Date.now(),automatic,received:Date.now(),monotonic:performance.now()});}
    finally{if(active.current===id){active.current=null;setPending(null);}}
  }
  return {result,pending,test,clear};
}
