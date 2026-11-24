import { useEffect, useState } from 'react';
import { errorMessage, json, request } from './api';
import type { Component } from './api';

type Version = { namespace: string; component_name: string; version: number; title: string; created_at: number };
type Document = Version & { specification: Record<string, unknown> };
const PAGE = 20;
const example = JSON.stringify({openapi:'3.0.3',info:{title:'Service interface',version:'1.0.0'},paths:{'/health':{get:{responses:{'200':{description:'Healthy'}}}}}},null,2);
const path = (v: Version) => '/api/interfaces/'+encodeURIComponent(v.namespace)+'/'+encodeURIComponent(v.component_name)+'/'+v.version;
const record = (v: unknown): Record<string, unknown> => v && typeof v === 'object' && !Array.isArray(v) ? v as Record<string, unknown> : {};
const methods = ['get','post','put','patch','delete','head','options','trace'];

export default function Interfaces({run,write,onSelect}:{run:typeof request;write:boolean;onSelect:(namespace:string,name:string)=>void}) {
  const [versions,setVersions]=useState<Version[]>([]);
  const [offset,setOffset]=useState(0);
  const [componentOffset,setComponentOffset]=useState(0);
  const [components,setComponents]=useState<Component[]>([]);
  const [selected,setSelected]=useState('');
  const [source,setSource]=useState(example);
  const [document,setDocument]=useState<Document|null>(null);
  const [error,setError]=useState('');
  const [notice,setNotice]=useState('');
  const [loading,setLoading]=useState(true);
  const [busy,setBusy]=useState(false);
  const [revision,setRevision]=useState(0);
  useEffect(()=>{
    const controller=new AbortController();
    run<Version[]>(`/api/interfaces?offset=${offset}&limit=${PAGE}`,{signal:controller.signal}).then(v=>{if(!controller.signal.aborted)setVersions(v);}).catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));}).finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[run,offset,revision]);
  useEffect(()=>{
    if(!write)return;
    const controller=new AbortController();
    run<Component[]>(`/api/components?offset=${componentOffset}&limit=${PAGE}`,{signal:controller.signal}).then(v=>{if(!controller.signal.aborted)setComponents(v);}).catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));});
    return ()=>controller.abort();
  },[run,write,componentOffset]);
  async function open(v:Version){setBusy(true);setError('');setDocument(null);try{setDocument(await run<Document>(path(v)));}catch(e){setError(errorMessage(e));}finally{setBusy(false);}}
  async function publish(){
    setBusy(true);setError('');setNotice('');
    try{
      if(!selected)throw new Error('Select a component.');
      if(new TextEncoder().encode(source).length>131072)throw new Error('Specification exceeds 128 KiB.');
      const [namespace,name]=selected.split('/');
      const v=await run<Version>('/api/interfaces/'+encodeURIComponent(namespace)+'/'+encodeURIComponent(name),json('POST',{specification:JSON.parse(source)}));
      setNotice('Published immutable version '+v.version+'.');setLoading(true);setOffset(0);setRevision(n=>n+1);setDocument(await run<Document>(path(v)));
    }catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }
  const spec=document?.specification;
  return <section aria-labelledby="interfaces-title"><h1 id="interfaces-title">Interface explorer</h1><p>Browse registered OpenAPI 3.0 documents. Passive preview only; no requests are sent to documented services.</p>
    {error&&<p role="alert" className="error">{error}</p>}{notice&&<p role="status">{notice}</p>}
    {write&&<form className="panel" onSubmit={e=>{e.preventDefault();void publish();}}><h2>Register specification</h2><label>Component<select required value={selected} onChange={e=>setSelected(e.target.value)}><option value="">Select a component</option>{components.map(c=><option key={c.namespace+'/'+c.name} value={c.namespace+'/'+c.name}>{c.namespace}/{c.name}</option>)}</select></label><div className="actions"><button type="button" disabled={!componentOffset||busy} onClick={()=>{setSelected('');setComponentOffset(n=>n-PAGE);}}>Previous components</button><button type="button" disabled={components.length<PAGE||busy} onClick={()=>{setSelected('');setComponentOffset(n=>n+PAGE);}}>Next components</button></div><label>OpenAPI JSON specification<textarea rows={12} value={source} onChange={e=>setSource(e.target.value)} required maxLength={131072}/></label><button className="primary" disabled={busy}>Publish new version</button></form>}
    <section className="panel" aria-label="Registered specifications"><h2>Registered specifications</h2>{loading?<p role="status">Loading specifications…</p>:versions.length===0?<p>No specifications registered.</p>:<ul>{versions.map(v=><li key={path(v)}><button disabled={busy} onClick={()=>void open(v)}>{v.title} — {v.namespace}/{v.component_name} — version {v.version}</button></li>)}</ul>}<div className="actions"><button disabled={loading||!offset} onClick={()=>{setLoading(true);setOffset(n=>n-PAGE);}}>Previous</button><button disabled={loading||versions.length<PAGE||offset>=10000} onClick={()=>{setLoading(true);setOffset(n=>n+PAGE);}}>Next</button><button onClick={()=>{setLoading(true);setError('');setRevision(n=>n+1);}}>Refresh</button></div></section>
    {busy&&<p role="status">Working…</p>}{document&&spec&&<section className="panel" aria-label="Specification preview"><h2>{document.title} · version {document.version}</h2><button onClick={()=>onSelect(document.namespace,document.component_name)}>View associated component</button><p>{String(record(spec.info).description??'')}</p><h3>Operations</h3>{Object.entries(record(spec.paths)).map(([name,item])=><section key={name}><h4>{name}</h4>{Object.entries(record(item)).filter(([method])=>methods.includes(method)).map(([method,op])=><details key={method}><summary>{method.toUpperCase()} {String(record(op).summary??'')}</summary><p>{String(record(op).description??'')}</p><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere'}}>{JSON.stringify(op,null,2)}</pre></details>)}</section>)}<h3>Schemas</h3>{Object.entries(record(record(spec.components).schemas)).map(([name,schema])=><details key={name}><summary>{name}</summary><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere'}}>{JSON.stringify(schema,null,2)}</pre></details>)}<details><summary>Full specification source</summary><pre style={{whiteSpace:'pre-wrap',overflowWrap:'anywhere'}}>{JSON.stringify(spec,null,2)}</pre></details></section>}
  </section>;
}
