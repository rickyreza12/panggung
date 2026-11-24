import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { request, json, errorMessage } from './api';

const checks = ['description', 'ownership', 'docs', 'metadata'] as const;
type Check = typeof checks[number];
type Standard = {name:string;version:number;title:string;checks:Check[]};
type Evaluation = {id:number;namespace:string;name:string;standard_name:string;standard_version:number;team:string|null;actor_id:number|null;created_at:number;results:{check:Check;status:'pass'|'fail'|'unknown';evidence:string}[]};
const labels:Record<Check,string>={description:'Component description',ownership:'Resolved owner',docs:'Verified documentation',metadata:'Supported metadata'};
const remediation:Record<Check,string>={description:'Add a meaningful component description explaining its purpose and responsibilities.',ownership:'Register the responsible team or user and correct the component owner reference.',docs:'Publish a valid documentation version; investigate storage integrity if an existing version cannot be verified.',metadata:'Import a supported Component descriptor with its type and lifecycle recorded.'};
const size=20;
function usePage<T>(run:typeof request,path:string,filter:string,revision:number){
 const [offset,setOffset]=useState(0);
 const [data,setData]=useState<T[]>([]);
 const [loading,setLoading]=useState(true);
 const [error,setError]=useState('');
 const [retry,setRetry]=useState(0);
 useEffect(()=>{
  const c=new AbortController();setLoading(true);setError('');
  run<T[]>(path+'?offset='+offset+'&limit='+(size+1)+filter,{signal:c.signal}).then(v=>{if(!c.signal.aborted)setData(v);}).catch(e=>{if(!c.signal.aborted)setError(errorMessage(e));}).finally(()=>{if(!c.signal.aborted)setLoading(false);});
  return ()=>c.abort();
 },[run,path,filter,revision,offset,retry]);
 return {items:data.slice(0,size),loading,error,offset,more:data.length>size,reset:()=>setOffset(0),next:()=>setOffset(n=>n+size),prev:()=>setOffset(n=>Math.max(0,n-size)),retry:()=>setRetry(n=>n+1)};
}
function Paging({page}:{page:{offset:number;loading:boolean;error:string;more:boolean;next:()=>void;prev:()=>void}}){
 return <div className="pagination"><span>Page {page.offset/size+1}</span><div className="actions"><button disabled={page.loading||page.offset===0} onClick={page.prev}>Previous</button><button disabled={page.loading||!!page.error||!page.more} onClick={page.next}>Next</button></div></div>;
}
function StandardForm({run,saved}:{run:typeof request;saved:()=>void}){
 const [selected,setSelected]=useState<Check[]>([...checks]);
 const [busy,setBusy]=useState(false);
 const [error,setError]=useState('');
 async function submit(e:FormEvent<HTMLFormElement>){
  e.preventDefault();if(busy)return;const form=e.currentTarget;const data=new FormData(form);
  setBusy(true);setError('');
  try{
   if(!selected.length)throw new Error('Choose at least one check.');
   await run<Standard>('/api/scorecards/standards',json('POST',{name:data.get('name'),version:Number(data.get('version')),title:data.get('title'),checks:selected}));
   form.reset();setSelected([...checks]);saved();
  }catch(e){setError(errorMessage(e));}finally{setBusy(false);}
 }
 return <section className="panel section"><h2>Configure standard version</h2><p>Choose evidence checks. Existing versions are immutable.</p><form className="form" onSubmit={submit} aria-busy={busy}><fieldset className="template-fields" disabled={busy}>
 <label>Standard name<input name="name" required maxLength={63}/></label><label>Version<input name="version" type="number" min={1} max={2147483647} step={1} required/></label><label>Title<input name="title" required maxLength={120}/></label>
 <fieldset className="template-param"><legend>Evidence checks</legend>{checks.map(c=><label className="template-check" key={c}><input type="checkbox" checked={selected.includes(c)} onChange={e=>setSelected(old=>e.target.checked?[...old,c]:old.filter(v=>v!==c))}/>{labels[c]}</label>)}</fieldset></fieldset>
 {error&&<p role="alert" className="error">{error}</p>}<button className="primary" disabled={busy||!selected.length}>{busy?'Saving…':'Create standard version'}</button></form></section>;
}
function EvaluateForm({run,standard,saved,cancel}:{run:typeof request;standard:Standard;saved:()=>void;cancel:()=>void}){
 const [busy,setBusy]=useState(false);const [error,setError]=useState('');
 async function submit(e:FormEvent<HTMLFormElement>){
  e.preventDefault();if(busy)return;const data=new FormData(e.currentTarget);setBusy(true);setError('');
  try{await run<Evaluation>('/api/scorecards/evaluations',json('POST',{namespace:data.get('namespace'),name:data.get('name'),standard_name:standard.name,standard_version:standard.version}));saved();}catch(e){setError(errorMessage(e));}finally{setBusy(false);}
 }
 return <section className="panel section"><h2>Evaluate component</h2><p>{standard.title} · version {standard.version}</p><form className="form" onSubmit={submit} aria-busy={busy}><fieldset className="template-fields" disabled={busy}><label>Component namespace<input name="namespace" required maxLength={63} defaultValue="default"/></label><label>Component name<input name="name" required maxLength={63}/></label></fieldset>{error&&<p className="error" role="alert">{error}</p>}<div className="actions"><button type="button" disabled={busy} onClick={cancel}>Cancel</button><button className="primary" disabled={busy}>{busy?'Evaluating…':'Evaluate and save'}</button></div></form></section>;
}
export default function Scorecards({run,admin,write}:{run:typeof request;admin:boolean;write:boolean}){
 const [revision,setRevision]=useState(0);
 const [team,setTeam]=useState('');const [filter,setFilter]=useState('');
 const standards=usePage<Standard>(run,'/api/scorecards/standards','',revision);
 const history=usePage<Evaluation>(run,'/api/scorecards/evaluations',filter?'&team='+encodeURIComponent(filter):'',revision);
 const [selected,setSelected]=useState<Standard|null>(null);const [notice,setNotice]=useState('');
 return <><div className="heading"><h1>Engineering scorecards</h1><p>Evaluate recorded evidence against a versioned standard. Unknown means evidence is unavailable, not a passing score.</p></div>
 {notice&&<p role="status" className="notice">{notice}</p>}
 {admin&&<StandardForm run={run} saved={()=>{setRevision(n=>n+1);setNotice('Standard version saved.');}}/>}
 <section className="panel section"><h2>Standard versions</h2>{standards.loading?<p role="status">Loading standards…</p>:standards.error?<><p className="error" role="alert">{standards.error}</p><button onClick={standards.retry}>Retry</button></>:standards.items.length===0?<p>No standard versions on this page.</p>:<ul className="relations">{standards.items.map(s=><li key={s.name+'/'+s.version}><strong>{s.title} · v{s.version}</strong><span>{s.checks.map(c=>labels[c]).join(', ')}</span>{write&&<button onClick={()=>setSelected(s)}>Evaluate component</button>}</li>)}</ul>}<Paging page={standards}/></section>
 {write&&selected&&<EvaluateForm key={selected.name+'/'+selected.version} run={run} standard={selected} cancel={()=>setSelected(null)} saved={()=>{setSelected(null);history.reset();setRevision(n=>n+1);setNotice('Evaluation saved.');}}/>}
 <section className="panel section"><h2>Evaluation history</h2><form className="filters" onSubmit={e=>{e.preventDefault();history.reset();setFilter(team.trim());}}><label>Team name (resolved ownership at evaluation time)<input value={team} maxLength={63} onChange={e=>setTeam(e.target.value)}/></label><button>Filter</button><button type="button" onClick={()=>{setTeam('');setFilter('');history.reset();}}>Clear</button></form>
 {history.loading?<p role="status">Loading evaluations…</p>:history.error?<><p className="error" role="alert">{history.error}</p><button onClick={history.retry}>Retry</button></>:history.items.length===0?<p>No recorded evaluations match this page.</p>:history.items.map(e=><section className="panel section" key={e.id}><h3>{e.namespace}/{e.name}</h3><p><code>{e.standard_name} v{e.standard_version}</code> · Evaluation #{e.id} · Actor {e.actor_id??'unavailable'} · {e.team??'No resolved team'} · {new Date(e.created_at*1000).toLocaleString()}</p><p>{e.results.filter(r=>r.status==='pass').length} pass · {e.results.filter(r=>r.status==='fail').length} fail · {e.results.filter(r=>r.status==='unknown').length} unknown</p><ul className="relations">{e.results.map(r=><li key={r.check}><strong>{labels[r.check]}: {r.status}</strong><span>{r.evidence}</span>{r.status!=='pass'&&<span><strong>Remediation:</strong> {remediation[r.check]}</span>}</li>)}</ul></section>)}<Paging page={history}/></section></>;
}
