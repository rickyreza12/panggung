import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import Markdown from 'react-markdown';
import { entityKey, errorMessage, json, listPath } from './api';
import type { Component, request } from './api';
import './techdocs.css';

interface Version { version: number; created_at: number; created_by: number | null }
interface DocumentVersion extends Version { markdown: string }
const PAGE_SIZE = 20;
export function versionsPath(namespace: string, name: string) {
  return '/api/techdocs/'+encodeURIComponent(namespace)+'/'+encodeURIComponent(name)+'/versions';
}
export function validateMarkdown(source: string) {
  if (new TextEncoder().encode(source).length > 65536) throw new Error('Documentation must not exceed 64 KiB.');
  const invalidControl = Array.from(source).some(char=>{const n=char.charCodeAt(0);return (n<32&&n!==9&&n!==10&&n!==13)||(n>=127&&n<=159);});
  if (!source.trim() || invalidControl) throw new Error('Enter nonempty Markdown without control characters.');
}
export function SafeMarkdown({ source }: {source: string}) {
  return <div className="docs-markdown"><Markdown skipHtml disallowedElements={['img']} unwrapDisallowed={false} components={{
    a: ({children}) => <span>{children}</span>,
  }}>{source}</Markdown></div>;
}
function VersionPanel({ component, run, write }: {component: Component; run: typeof request; write: boolean}) {
  const path=versionsPath(component.namespace,component.name);
  const [versions,setVersions]=useState<Version[]>([]);
  const [selected,setSelected]=useState<number|null>(null);
  const [document,setDocument]=useState<DocumentVersion|null>(null);
  const [loading,setLoading]=useState(true);
  const [reading,setReading]=useState(false);
  const [error,setError]=useState('');
  const [readError,setReadError]=useState('');
  const [publishError,setPublishError]=useState('');
  const [notice,setNotice]=useState('');
  const [draft,setDraft]=useState('');
  const [busy,setBusy]=useState(false);
  const [refresh,setRefresh]=useState(0);
  const [retryRead,setRetryRead]=useState(0);
  useEffect(()=>{
    const controller=new AbortController();setLoading(true);setError('');
    run<Version[]>(path,{signal:controller.signal}).then(data=>{
      if(controller.signal.aborted)return;
      setVersions(data);
      setSelected(previous=>previous!==null&&data.some(v=>v.version===previous)?previous:data[0]?.version??null);
    }).catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[path,run,refresh]);
  useEffect(()=>{
    const controller=new AbortController();setDocument(null);setReadError('');
    if(selected===null){setReading(false);return ()=>controller.abort();}
    setReading(true);
    run<DocumentVersion>(path+'/'+selected,{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setDocument(data);})
      .catch(e=>{if(!controller.signal.aborted)setReadError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setReading(false);});
    return ()=>controller.abort();
  },[selected,path,run,retryRead]);
  async function publish(event: FormEvent) {
    event.preventDefault();if(!write||busy)return;
    setBusy(true);setPublishError('');setNotice('');
    try {
      validateMarkdown(draft);
      const version=await run<Version>(path,json('POST',{markdown:draft}));
      setDraft('');setSelected(version.version);setRefresh(n=>n+1);
      setNotice('Version '+version.version+' published.');
    } catch(e){setPublishError(errorMessage(e));}finally{setBusy(false);}
  }
  return <section className="panel section">
    <h2>{entityKey(component)}</h2>
    <p className="muted">Published versions are immutable. Publishing creates a new version.</p>
    {notice&&<p className="notice" role="status">{notice}</p>}
    {loading?<p role="status">Loading versions…</p>:error?<><p role="alert" className="error">{error}</p><button onClick={()=>setRefresh(n=>n+1)}>Retry versions</button></>:versions.length===0?<p>No documentation published.</p>:<>
      <label>Published version<select value={selected??''} onChange={e=>setSelected(Number(e.target.value))}>
        {versions.map((v,i)=><option key={v.version} value={v.version}>Version {v.version}{i===0?' (latest)':''} · {new Date(v.created_at*1000).toLocaleString()}</option>)}
      </select></label>
      {versions.length===100&&<p className="muted">Showing the latest 100 versions.</p>}
      {reading?<p role="status">Loading documentation…</p>:readError?<><p className="error" role="alert">{readError}</p><button onClick={()=>setRetryRead(n=>n+1)}>Retry document</button></>:document&&document.version===selected?<article aria-label={'Documentation version '+document.version}><p className="muted">Version {document.version} · Published {new Date(document.created_at*1000).toLocaleString()}</p><SafeMarkdown source={document.markdown}/></article>:null}
    </>}
    {write&&<form className="form docs-publish" onSubmit={publish}>
      <h3>Publish new version</h3><label>Markdown source<textarea rows={12} value={draft} onChange={e=>setDraft(e.target.value)} required disabled={busy} spellCheck={false}/></label>
      <p className="muted">{new TextEncoder().encode(draft).length} / 65536 bytes. Links are displayed as text; images and raw markup are omitted.</p>
      {publishError&&<p className="error" role="alert">{publishError}</p>}
      <button className="primary" disabled={busy||!draft.trim()||loading||!!error}>{busy?'Publishing…':'Publish new version'}</button>
    </form>}
  </section>;
}
export default function TechDocs({run,write}:{run:typeof request;write:boolean}) {
  const heading=useRef<HTMLHeadingElement>(null);
  const [components,setComponents]=useState<Component[]>([]);
  const [selected,setSelected]=useState<Component|null>(null);
  const [draft,setDraft]=useState('');
  const [query,setQuery]=useState('');
  const [offset,setOffset]=useState(0);
  const [loading,setLoading]=useState(true);
  const [error,setError]=useState('');
  const [retry,setRetry]=useState(0);
  useEffect(()=>{heading.current?.focus();},[]);
  useEffect(()=>{
    const controller=new AbortController();setLoading(true);setError('');
    run<Component[]>(listPath({q:query,namespace:''},offset,PAGE_SIZE+1),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setComponents(data);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[query,offset,run,retry]);
  return <>
    <div className="heading"><h1 ref={heading} tabIndex={-1}>Technical documentation</h1><p>Read and publish versioned documentation for catalog components.</p></div>
    <div className="docs-layout">
      <section className="panel section">
        <h2>Components</h2>
        <form className="form" onSubmit={e=>{e.preventDefault();setQuery(draft.trim());setOffset(0);}}>
          <label>Search components<input type="search" value={draft} maxLength={200} onChange={e=>setDraft(e.target.value)}/></label><button>Search</button>
        </form>
        {loading?<p role="status">Loading components…</p>:error?<><p role="alert" className="error">{error}</p><button onClick={()=>setRetry(n=>n+1)}>Retry components</button></>:components.length===0?<p>No components on this page.</p>:<ul className="docs-components">{components.slice(0,PAGE_SIZE).map(c=><li key={entityKey(c)}><button aria-pressed={selected!==null&&entityKey(selected)===entityKey(c)} onClick={()=>setSelected(c)}>{entityKey(c)}</button></li>)}</ul>}
        <div className="actions"><button disabled={loading||offset===0} onClick={()=>setOffset(n=>Math.max(0,n-PAGE_SIZE))}>Previous</button><button disabled={loading||!!error||components.length<=PAGE_SIZE} onClick={()=>setOffset(n=>n+PAGE_SIZE)}>Next</button></div>
        <p className="muted">Page {Math.floor(offset/PAGE_SIZE)+1}</p>
      </section>
      {selected?<VersionPanel key={entityKey(selected)} component={selected} run={run} write={write}/>:<section className="panel section"><h2>Select a component</h2><p>Choose a catalog component to view its documentation.</p></section>}
    </div>
  </>;
}
