import {useEffect,useRef,useState} from 'react';
import {errorMessage} from './api';
import type {request} from './api';
import './search.css';

interface Hit {namespace:string;name:string;description:string;owner:string;component_type:string|null;lifecycle:string|null}
interface Page {items:Hit[];has_more:boolean}
const PAGE_SIZE=20;
export function searchPath(q:string,offset:number) {
  return '/api/search?'+new URLSearchParams({q,offset:String(offset),limit:String(PAGE_SIZE)}).toString();
}
export default function Search({run,admin,onSelect}:{run:typeof request;admin:boolean;onSelect:(namespace:string,name:string)=>void}) {
  const heading=useRef<HTMLHeadingElement>(null);
  const [draft,setDraft]=useState('');
  const [query,setQuery]=useState('');
  const [offset,setOffset]=useState(0);
  const [page,setPage]=useState<Page|null>(null);
  const [loading,setLoading]=useState(false);
  const [error,setError]=useState('');
  const [rebuildError,setRebuildError]=useState('');
  const [notice,setNotice]=useState('');
  const [busy,setBusy]=useState(false);
  const [retry,setRetry]=useState(0);
  useEffect(()=>{heading.current?.focus();},[]);
  useEffect(()=>{
    const controller=new AbortController();
    setPage(null);setError('');
    if(!query){setLoading(false);return ()=>controller.abort();}
    setLoading(true);
    run<Page>(searchPath(query,offset),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setPage(data);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[query,offset,retry,run]);
  async function rebuild(){
    if(!admin||busy)return;
    setBusy(true);setRebuildError('');setNotice('');
    try {await run<void>('/api/search/rebuild',{method:'POST'});setNotice('Component search index rebuilt.');setRetry(n=>n+1);}
    catch(e){setRebuildError(errorMessage(e));}finally{setBusy(false);}
  }
  return <>
    <div className="page-top"><div className="heading"><h1 ref={heading} tabIndex={-1}>Search catalog</h1><p>Find components by name, namespace, description, owner, type or lifecycle. Documentation and other entity types are not indexed.</p></div>
      {admin&&<button disabled={busy} onClick={rebuild}>{busy?'Rebuilding…':'Rebuild search index'}</button>}
    </div>
    {rebuildError&&<p className="error" role="alert">{rebuildError}</p>}{notice&&<p className="notice" role="status">{notice}</p>}
    <section className="panel section">
      <form className="filters search-form" onSubmit={e=>{e.preventDefault();setOffset(0);setQuery(draft.trim());setRetry(n=>n+1);}}>
        <label>Search terms<input type="search" required maxLength={200} value={draft} onChange={e=>setDraft(e.target.value)} placeholder="Billing service"/></label><button className="primary" disabled={!draft.trim()}>Search</button>
      </form>
      <p className="muted">All words must match. Search terms are treated literally; advanced operators are not supported.</p>
      {loading?<p role="status">Searching components…</p>:error?<><p className="error" role="alert">{error}</p><button onClick={()=>setRetry(n=>n+1)}>Retry search</button></>:!query?<p>Enter words to search registered components.</p>:page&&page.items.length===0?<p>No matching components on this page.</p>:page?<ul className="search-results">{page.items.map(item=><li key={item.namespace+'/'+item.name}><button className="text-link" onClick={()=>onSelect(item.namespace,item.name)}>{item.name}</button><code>{item.namespace}</code><p>{item.description||'No description provided.'}</p><small>Owner: {item.owner} · {item.component_type??'Unspecified type'} · {item.lifecycle??'Unspecified lifecycle'}</small></li>)}</ul>:null}
      {query&&<div className="pagination"><span>Page {Math.floor(offset/PAGE_SIZE)+1}</span><div className="actions"><button disabled={loading||offset===0} onClick={()=>setOffset(n=>Math.max(0,n-PAGE_SIZE))}>Previous</button><button disabled={loading||!page?.has_more||!!error} onClick={()=>setOffset(n=>n+PAGE_SIZE)}>Next</button></div></div>}
    </section>
  </>;
}
