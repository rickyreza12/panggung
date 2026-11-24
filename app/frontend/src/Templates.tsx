import './templates.css';
import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { errorMessage, json, request } from './api';

type Parameter = { name: string; label: string; type: 'string' | 'boolean' | 'integer'; required: boolean; description?: string };
type Template = { name: string; version: string; title: string; description: string; stack: string; parameters: Parameter[] };
const stacks = ['rust', 'dotnet', 'python', 'node', 'react'];
const pageSize = 20;
const initial = (): Template => ({ name: '', version: '1.0.0', title: '', description: '', stack: 'rust', parameters: [] });
const detailPath = (name: string, version: string) => '/api/templates/' + encodeURIComponent(name) + '/' + encodeURIComponent(version);

function TemplateForm({ run, source, onSaved, cancel }: { run: typeof request; source?: Template; onSaved: (item: Template) => void; cancel: () => void }) {
  const [draft, setDraft] = useState<Template>(() => source ? { ...source, version: '', parameters: source.parameters.map(p => ({...p})) } : initial());
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  function parameter(index: number, change: Partial<Parameter>) {
    setDraft(old => ({...old, parameters: old.parameters.map((p,i) => i === index ? {...p,...change} : p)}));
  }
  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true); setError('');
    try {
      if (new Set(draft.parameters.map(p=>p.name)).size !== draft.parameters.length) throw new Error('Parameter names must be unique.');
      onSaved(await run<Template>('/api/templates', json('POST', draft)));
    } catch (err) { setError(errorMessage(err)); } finally { setBusy(false); }
  }
  return <section className="panel section"><h2>{source ? 'Create a new version' : 'Create template version'}</h2><p>Published versions cannot be edited or overwritten.</p>
    <form className="form" onSubmit={submit} aria-busy={busy}>
      <fieldset className="template-fields" disabled={busy}>
        <label>Name<input required maxLength={63} readOnly={!!source} value={draft.name} onChange={e=>setDraft({...draft,name:e.target.value})}/></label>
        <label>Version<input required maxLength={29} placeholder="1.0.0" value={draft.version} onChange={e=>setDraft({...draft,version:e.target.value})}/></label>
        <label>Title<input required maxLength={120} value={draft.title} onChange={e=>setDraft({...draft,title:e.target.value})}/></label>
        <label>Stack<select value={draft.stack} onChange={e=>setDraft({...draft,stack:e.target.value})}>{stacks.map(s=><option key={s}>{s}</option>)}</select></label>
        <label>Description<textarea maxLength={2000} rows={4} value={draft.description} onChange={e=>setDraft({...draft,description:e.target.value})}/></label>
        <h3>Parameter definitions</h3><p className="muted">Up to 20 parameters. These are declarations, not execution steps.</p>
        {draft.parameters.map((p,i)=><fieldset className="template-param" key={i}><legend>Parameter {i+1}</legend>
          <label>Name<input required maxLength={63} value={p.name} onChange={e=>parameter(i,{name:e.target.value})}/></label>
          <label>Label<input required maxLength={120} value={p.label} onChange={e=>parameter(i,{label:e.target.value})}/></label>
          <label>Type<select value={p.type} onChange={e=>parameter(i,{type:e.target.value as Parameter['type']})}><option>string</option><option>boolean</option><option>integer</option></select></label>
          <label>Description<input maxLength={500} value={p.description??''} onChange={e=>parameter(i,{description:e.target.value})}/></label>
          <label className="template-check"><input type="checkbox" checked={p.required} onChange={e=>parameter(i,{required:e.target.checked})}/> Required</label>
          <button type="button" onClick={()=>setDraft(old=>({...old,parameters:old.parameters.filter((_,index)=>index!==i)}))}>Remove parameter {i+1}</button>
        </fieldset>)}
        <button type="button" disabled={draft.parameters.length>=20} onClick={()=>setDraft(old=>({...old,parameters:[...old.parameters,{name:'',label:'',type:'string',required:false}]}))}>Add parameter</button>
      </fieldset>
      {error&&<p role="alert" className="error">{error}</p>}
      <div className="actions"><button type="button" disabled={busy} onClick={cancel}>Cancel</button><button className="primary" disabled={busy}>{busy?'Creating…':'Publish version'}</button></div>
    </form></section>;
}

export default function Templates({run, admin}:{run:typeof request;admin:boolean}) {
  const [items,setItems]=useState<Template[]>([]);
  const [offset,setOffset]=useState(0);
  const [reload,setReload]=useState(0);
  const [loading,setLoading]=useState(true);
  const [error,setError]=useState('');
  const [selected,setSelected]=useState<{name:string;version:string}|null>(null);
  const [detail,setDetail]=useState<Template|null>(null);
  const [detailLoading,setDetailLoading]=useState(false);
  const [detailError,setDetailError]=useState('');
  const [form,setForm]=useState<'new'|'version'|null>(null);
  const [notice,setNotice]=useState('');
  useEffect(()=>{
    const controller=new AbortController();setLoading(true);setError('');
    run<Template[]>('/api/templates?offset='+offset+'&limit='+(pageSize+1),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setItems(data);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[run,offset,reload]);
  useEffect(()=>{
    if(!selected)return;
    const controller=new AbortController();setDetail(null);setDetailLoading(true);setDetailError('');
    run<Template>(detailPath(selected.name,selected.version),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setDetail(data);})
      .catch(e=>{if(!controller.signal.aborted)setDetailError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setDetailLoading(false);});
    return ()=>controller.abort();
  },[run,selected,reload]);
  function saved(item:Template){setForm(null);setNotice('Template version created.');setSelected({name:item.name,version:item.version});setReload(n=>n+1);}
  return <>
    <div className="page-top"><div className="heading"><h1>Software templates</h1><p>Browse versioned starter definitions. Publishing a version does not run or deploy code.</p></div>{admin&&!form&&<button className="primary" onClick={()=>{setNotice('');setForm('new');}}>Create template</button>}</div>
    {notice&&<p role="status" className="notice">{notice}</p>}
    {admin&&form&&<TemplateForm key={form} run={run} source={form==='version'?detail??undefined:undefined} onSaved={saved} cancel={()=>setForm(null)}/>}
    {!form&&selected&&<section className="panel section"><button className="text-link" onClick={()=>{setSelected(null);setNotice('');}}>← All templates</button>
      {detailLoading?<p role="status">Loading template version…</p>:detailError?<><p role="alert" className="error">{detailError}</p><button onClick={()=>setReload(n=>n+1)}>Retry</button></>:detail&&<>
        <div className="page-top"><div><h2>{detail.title}</h2><p><code>{detail.name} · {detail.version}</code></p></div>{admin&&<button onClick={()=>setForm('version')}>Create new version</button>}</div>
        <p>{detail.description||'No description provided.'}</p><p>Stack: {detail.stack}</p><h3>Parameters</h3>
        {detail.parameters.length===0?<p>No parameters declared.</p>:<ul className="relations">{detail.parameters.map(p=><li key={p.name}><strong>{p.label}</strong><code>{p.name}</code><span>{p.type} · {p.required?'Required':'Optional'}</span>{p.description&&<small>{p.description}</small>}</li>)}</ul>}
      </>}
    </section>}
    {!form&&!selected&&<section className="panel"><div className="table-title"><h2>Published versions</h2><span role="status">{loading?'Loading templates…':error?'Unavailable':items.length?'Showing '+(offset+1)+'–'+(offset+Math.min(pageSize,items.length)):'No versions on this page'}</span></div>
      {error?<div className="empty"><p role="alert">{error}</p><button onClick={()=>setReload(n=>n+1)}>Retry</button></div>:loading?<p className="empty">Retrieving templates…</p>:items.length===0?<p className="empty">No template versions on this page.</p>:<div className="table-scroll" tabIndex={0} role="region" aria-label="Template versions"><table><thead><tr><th scope="col">Template</th><th scope="col">Version</th><th scope="col">Stack</th></tr></thead><tbody>{items.slice(0,pageSize).map(item=><tr key={item.name+'/'+item.version}><td><button className="text-link" onClick={()=>setSelected({name:item.name,version:item.version})}>{item.title}</button><small>{item.name}</small></td><td><code>{item.version}</code></td><td>{item.stack}</td></tr>)}</tbody></table></div>}
      <div className="pagination"><span>Page {offset/pageSize+1}</span><div className="actions"><button disabled={loading||offset===0} onClick={()=>setOffset(n=>n-pageSize)}>Previous</button><button disabled={loading||!!error||items.length<=pageSize} onClick={()=>setOffset(n=>n+pageSize)}>Next</button></div></div>
    </section>}
  </>;
}
