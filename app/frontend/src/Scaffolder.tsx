import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { request, json, errorMessage } from './api';

type Parameter = { name: string; label: string; type: 'string' | 'boolean' | 'integer'; required: boolean; description?: string };
type Template = { name: string; version: string; title: string; stack: string; parameters: Parameter[] };
type Artifact = { name: string; version: string; stack: string; files: Record<string,string> };
type Job = { id: number; actor_id: number; name: string; version: string; created_at: number };
const size = 20;

function usePage<T>(run: typeof request, path: string, revision: number) {
  const [offset,setOffset] = useState(0);
  const [items,setItems] = useState<T[]>([]);
  const [error,setError] = useState('');
  const [loading,setLoading] = useState(true);
  const [retry,setRetry] = useState(0);
  useEffect(()=>{
    const controller = new AbortController();
    setLoading(true); setError('');
    run<T[]>(path+'?offset='+offset+'&limit='+(size+1),{signal:controller.signal})
      .then(result=>{if(!controller.signal.aborted)setItems(result);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[run,path,revision,offset,retry]);
  return {items:items.slice(0,size),error,loading,offset,next:items.length>size,previous:()=>setOffset(n=>Math.max(0,n-size)),advance:()=>setOffset(n=>n+size),retry:()=>setRetry(n=>n+1),reset:()=>setOffset(0)};
}
function Paging({offset, next, loading, error, previous, advance}: {offset:number;next:boolean;loading:boolean;error:string;previous:()=>void;advance:()=>void}) {
  return <div className="pagination"><span>Page {offset/size+1}</span><div className="actions"><button disabled={loading||offset===0} onClick={previous}>Previous</button><button disabled={loading||!!error||!next} onClick={advance}>Next</button></div></div>;
}
function Generate({template,run,onCreated,cancel}:{template:Template;run:typeof request;onCreated:(job:Job)=>void;cancel:()=>void}) {
  const [values,setValues]=useState<Record<string,string>>({});
  const [preview,setPreview]=useState<Artifact|null>(null);
  const [busy,setBusy]=useState(false);
  const [error,setError]=useState('');
  const [job,setJob]=useState<Job|null>(null);
  function change(name:string,value:string){setValues(old=>({...old,[name]:value}));setPreview(null);setJob(null);setError('');}
  function payload(){
    const parameters: Record<string,string|number|boolean> = Object.create(null);
    for(const p of template.parameters){
      const value=values[p.name]??'';
      if(value===''){if(p.required)throw new Error(p.label+' is required.');continue;}
      if(p.type==='boolean'){if(value!=='true'&&value!=='false')throw new Error('Invalid boolean value.');parameters[p.name]=value==='true';}
      else if(p.type==='integer'){const number=Number(value);if(!Number.isInteger(number)||number < -2147483648||number > 2147483647)throw new Error(p.label+' must be a 32-bit integer.');parameters[p.name]=number;}
      else {if(new TextEncoder().encode(value).length>2000||/\p{Cc}/u.test(value)||(p.required&&!value.trim()))throw new Error('Invalid text in '+p.label+'.');parameters[p.name]=value;}
    }
    return {name:template.name,version:template.version,parameters};
  }
  async function generate(save:boolean){
    if(busy)return;
    setBusy(true);setError('');
    try{
      const body=payload();
      if(save){const result=await run<Job>('/api/scaffolder/jobs',json('POST',body));setJob(result);onCreated(result);}
      else {setPreview(await run<Artifact>('/api/scaffolder/dry-run',json('POST',body)));setJob(null);}
    }catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }
  function submit(e:FormEvent){e.preventDefault();void generate(false);}
  return <section className="panel section"><h2>{template.title}</h2><p><code>{template.name} / {template.version}</code> · {template.stack}</p>
    <p>Preview the fixed starter files before saving. Parameter values are stored in the generated metadata; do not enter secrets.</p>
    <form className="form" onSubmit={submit} aria-busy={busy}>
      <fieldset className="template-fields" disabled={busy}>
      {template.parameters.map(p=><label key={p.name}>{p.label}{p.required?' (required)':''}
        {p.type==='boolean'?<select required={p.required} value={values[p.name]??''} onChange={e=>change(p.name,e.target.value)}><option value="">Select {p.required?'a value':'or leave unset'}</option><option value="true">True</option><option value="false">False</option></select>:
          <input required={p.required} type={p.type==='integer'?'number':'text'} min={p.type==='integer'?-2147483648:undefined} max={p.type==='integer'?2147483647:undefined} step={p.type==='integer'?1:undefined} maxLength={p.type==='string'?2000:undefined} value={values[p.name]??''} onChange={e=>change(p.name,e.target.value)}/>}
        {p.description&&<small>{p.description}</small>}
      </label>)}
      </fieldset>
      {error&&<p className="error" role="alert">{error}</p>}
      <div className="actions"><button type="button" disabled={busy} onClick={cancel}>Cancel</button><button className="primary" disabled={busy}>{busy?'Working…':'Preview files'}</button></div>
    </form>
    {preview&&<section><h3>Preview — not saved</h3>{Object.entries(preview.files).map(([path,content])=><details key={path}><summary><code>{path}</code></summary><pre>{content}</pre></details>)}<div className="actions"><button disabled={busy||!!job} onClick={()=>void generate(true)}>Save generated artifact</button></div></section>}
    {job&&<p className="notice" role="status">Job {job.id} saved. <a href={'/api/scaffolder/jobs/'+job.id+'/archive'} download>Download archive</a></p>}
  </section>;
}
export default function Scaffolder({run,write}:{run:typeof request;write:boolean}){
  const [revision,setRevision]=useState(0);
  const templates=usePage<Template>(run,'/api/templates',0);
  const jobs=usePage<Job>(run,'/api/scaffolder/jobs',revision);
  const [selected,setSelected]=useState<Template|null>(null);
  return <>
    <div className="heading"><h1>Software scaffolder</h1><p>Generate starter files locally. No commands, deployment or publishing are performed.</p></div>
    {write?<>{selected?<Generate key={selected.name+'/'+selected.version} template={selected} run={run} cancel={()=>setSelected(null)} onCreated={()=>{jobs.reset();setRevision(n=>n+1);}}/>:
      <section className="panel section"><h2>Choose a template version</h2>
        {templates.loading?<p role="status">Loading templates…</p>:templates.error?<><p role="alert" className="error">{templates.error}</p><button onClick={templates.retry}>Retry</button></>:templates.items.length===0?<p>No templates on this page.</p>:<ul className="relations">{templates.items.map(t=><li key={t.name+'/'+t.version}><button className="text-link" onClick={()=>setSelected(t)}>{t.title} · {t.version}</button><span>{t.stack}</span></li>)}</ul>}
        <Paging {...templates}/>
      </section>}</>:<p>Read-only access. Members and administrators can generate starters.</p>}
    <section className="panel"><div className="table-title"><h2>Job history</h2><button onClick={jobs.retry} disabled={jobs.loading}>Refresh</button></div>
      {jobs.loading?<p className="empty" role="status">Loading jobs…</p>:jobs.error?<p className="error" role="alert">{jobs.error}</p>:jobs.items.length===0?<p className="empty">No saved jobs on this page.</p>:<div className="table-scroll" role="region" aria-label="Saved scaffolder jobs" tabIndex={0}><table><thead><tr><th scope="col">Job</th><th scope="col">Template</th><th scope="col">Created</th><th scope="col">Downloads</th></tr></thead><tbody>{jobs.items.map(j=><tr key={j.id}><td>#{j.id}</td><td>{j.name} · {j.version}</td><td>{new Date(j.created_at*1000).toLocaleString()}</td><td><div className="actions"><a href={'/api/scaffolder/jobs/'+j.id+'/archive'} download aria-label={'Download archive for job '+j.id}>TAR archive</a><a href={'/api/scaffolder/jobs/'+j.id+'/artifact'} download aria-label={'Download manifest for job '+j.id}>JSON manifest</a></div></td></tr>)}</tbody></table></div>}
      <Paging {...jobs}/>
    </section>
  </>;
}
