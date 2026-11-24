import {useEffect,useRef,useState} from 'react';
import {errorMessage,json} from './api';
import type {request} from './api';

interface Repository {github_id:number;owner:string;name:string;description:string|null;default_branch:string;archived:boolean}
interface Registered extends Repository {descriptor_status:'found'|'missing'|'unsupported';synced_at:number;synced_by:number|null}
interface Page<T> {items:T[];has_more:boolean}
const LIMIT=20;
export default function GitHub({run,write,admin}:{run:typeof request;write:boolean;admin:boolean}) {
  const heading=useRef<HTMLHeadingElement>(null);
  const [owner,setOwner]=useState('');
  const [query,setQuery]=useState('');
  const [page,setPage]=useState(1);
  const [offset,setOffset]=useState(0);
  const [discovered,setDiscovered]=useState<Page<Repository>|null>(null);
  const [registered,setRegistered]=useState<Page<Registered>|null>(null);
  const [loading,setLoading]=useState(false);
  const [registryLoading,setRegistryLoading]=useState(true);
  const [error,setError]=useState('');
  const [registryError,setRegistryError]=useState('');
  const [syncError,setSyncError]=useState('');
  const [notice,setNotice]=useState('');
  const [busy,setBusy]=useState(false);
  const [retry,setRetry]=useState(0);
  const [refresh,setRefresh]=useState(0);
  useEffect(()=>{heading.current?.focus();},[]);
  useEffect(()=>{
    const controller=new AbortController();setError('');setDiscovered(null);
    if(!query){setLoading(false);return ()=>controller.abort();}
    setLoading(true);
    run<Page<Repository>>('/api/github/discover/'+encodeURIComponent(query)+'?'+new URLSearchParams({page:String(page),limit:String(LIMIT)}),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setDiscovered(data);})
      .catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setLoading(false);});
    return ()=>controller.abort();
  },[run,query,page,retry]);
  useEffect(()=>{
    const controller=new AbortController();setRegistryLoading(true);setRegistryError('');setRegistered(null);
    run<Page<Registered>>('/api/github/repositories?'+new URLSearchParams({offset:String(offset),limit:String(LIMIT)}),{signal:controller.signal})
      .then(data=>{if(!controller.signal.aborted)setRegistered(data);})
      .catch(e=>{if(!controller.signal.aborted)setRegistryError(errorMessage(e));})
      .finally(()=>{if(!controller.signal.aborted)setRegistryLoading(false);});
    return ()=>controller.abort();
  },[run,offset,refresh]);
  async function sync(repo:Repository){
    if(!write||busy)return;
    setBusy(true);setSyncError('');setNotice('');
    try {const saved=await run<Registered>('/api/github/sync',json('POST',{owner:repo.owner,repo:repo.name}));setNotice(saved.owner+'/'+saved.name+' synchronized.');setRefresh(n=>n+1);}
    catch(e){setSyncError(errorMessage(e));}finally{setBusy(false);}
  }
  function row(repo:Repository){return <><strong>{repo.owner}/{repo.name}</strong>{repo.archived&&<span> · Archived</span>}<p>{repo.description||'No description provided.'}</p><small>Default branch: {repo.default_branch}</small></>;}
  return <>
    <div className="heading"><h1 ref={heading} tabIndex={-1}>GitHub repositories</h1><p>Discover public repositories and explicitly synchronize metadata into Panggung. No remote writes or automatic catalog imports.</p></div>
    {notice&&<p role="status" className="notice">{notice}</p>}{syncError&&<p role="alert" className="error">{syncError}</p>}
    <section className="panel section">
      <h2>Discover public repositories</h2>
      <form className="filters" onSubmit={e=>{e.preventDefault();setQuery(owner.trim());setPage(1);setRetry(n=>n+1);}}>
        <label>GitHub owner<input required maxLength={39} value={owner} onChange={e=>setOwner(e.target.value)} placeholder="Organization or username"/></label>
        <button className="primary" disabled={!owner.trim()}>Discover</button>
      </form>
      {loading?<p role="status">Discovering repositories…</p>:error?<><p role="alert" className="error">{error}</p><button onClick={()=>setRetry(n=>n+1)}>Retry discovery</button></>:discovered?<>{discovered.items.length===0?<p>No public repositories on this page.</p>:<ul className="relations">{discovered.items.map(repo=><li key={repo.github_id}><div>{row(repo)}</div>{write&&<button disabled={busy} onClick={()=>sync(repo)}>Sync repository</button>}</li>)}</ul>}</>:<p>Enter an owner to discover repositories.</p>}
      {query&&<div className="pagination"><span>Page {page}</span><div className="actions"><button disabled={loading||page===1} onClick={()=>setPage(n=>n-1)}>Previous</button><button disabled={loading||!discovered?.has_more} onClick={()=>setPage(n=>n+1)}>Next</button></div></div>}
    </section>
    <section className="panel section"><h2>Synced repository registry</h2>
      <p className="muted">Descriptor discovery checks only the conventional root catalog-info.yaml file; found means a candidate exists, not that its contents are valid.</p>
      {registryLoading?<p role="status">Loading registry…</p>:registryError?<><p role="alert" className="error">{registryError}</p><button onClick={()=>setRefresh(n=>n+1)}>Retry registry</button></>:registered?.items.length?<ul className="relations">{registered.items.map(repo=><li key={repo.github_id}><div>{row(repo)}<p>Descriptor: {repo.descriptor_status} · Last sync: {new Date(repo.synced_at*1000).toLocaleString()}</p></div>{write&&<button disabled={busy} onClick={()=>sync(repo)}>Sync again</button>}</li>)}</ul>:<p>No repositories synchronized on this page.</p>}
      <div className="pagination"><span>Page {Math.floor(offset/LIMIT)+1}</span><div className="actions"><button disabled={registryLoading||offset===0} onClick={()=>setOffset(n=>Math.max(0,n-LIMIT))}>Previous</button><button disabled={registryLoading||!registered?.has_more} onClick={()=>setOffset(n=>n+LIMIT)}>Next</button></div></div>
    </section>
    {admin&&<><PublishSettings run={run}/><WebhookSettings run={run}/></>}
  </>;
}

interface Publication {owner:string;repo:string;state:'uncertain'|'succeeded';github_id:number|null;created_at:number}
interface PublicationPage {enabled:boolean;allowed_organizations:string[];items:Publication[];has_more:boolean}
function PublishSettings({run}:{run:typeof request}){
  const [data,setData]=useState<PublicationPage|null>(null);
  const [owner,setOwner]=useState('');const [repo,setRepo]=useState('');const [confirmation,setConfirmation]=useState('');
  const [offset,setOffset]=useState(0);const [refresh,setRefresh]=useState(0);const [busy,setBusy]=useState(false);const [error,setError]=useState('');const [notice,setNotice]=useState('');
  useEffect(()=>{const controller=new AbortController();run<PublicationPage>('/api/github/publish?offset='+offset+'&limit=20',{signal:controller.signal}).then(value=>{if(!controller.signal.aborted)setData(value);}).catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));});return ()=>controller.abort();},[run,offset,refresh]);
  async function submit(e:React.FormEvent){e.preventDefault();if(!data?.enabled||busy||confirmation!==owner+'/'+repo)return;setBusy(true);setError('');setNotice('');try{await run('/api/github/publish',json('POST',{owner,repo,confirmation}));setNotice('Private repository created.');setRepo('');}catch(e){setError(errorMessage(e)+' Inspect publication history and the destination before taking further action. Do not retry uncertain outcomes.');}finally{setConfirmation('');setBusy(false);setOffset(0);setRefresh(n=>n+1);}}
  return <section className="panel section"><h2>Create private repository</h2><p>Explicitly create an empty private GitHub repository in an allowed organization. Project file publishing is not yet supported. A reserved destination cannot be retried automatically.</p>{error&&<p role="alert" className="error">{error}</p>}{notice&&<p role="status">{notice}</p>}{data?<><p role="status">Publishing: {data.enabled?'Enabled':'Disabled — configure a dedicated write credential and organization allowlist on the server.'}</p>{data.enabled&&<form onSubmit={submit}><label>Destination organization<select required value={owner} onChange={e=>{setOwner(e.target.value);setConfirmation('');}}><option value="">Select organization</option>{data.allowed_organizations.map(org=><option key={org}>{org}</option>)}</select></label><label>Repository name<input required maxLength={100} pattern="[A-Za-z0-9_.\u002d]+" value={repo} onChange={e=>{setRepo(e.target.value);setConfirmation('');}}/></label><label>Confirm exact destination<input required maxLength={140} value={confirmation} onChange={e=>setConfirmation(e.target.value)} placeholder={owner+'/'+repo}/></label><button className="primary" disabled={busy||!owner||!repo||confirmation!==owner+'/'+repo}>Create private repository</button></form>}<h3>Publication history</h3>{data.items.length?<ul className="relations">{data.items.map(item=><li key={item.owner+'/'+item.repo}><strong>{item.owner}/{item.repo}</strong><span>{item.state==='uncertain'?'Outcome uncertain — inspect manually; do not retry':'Created'}</span></li>)}</ul>:<p>No publication attempts on this page.</p>}<div className="actions"><button disabled={busy||offset===0} onClick={()=>setOffset(n=>Math.max(0,n-20))}>Previous</button><button disabled={busy||!data.has_more} onClick={()=>setOffset(n=>n+20)}>Next</button><button disabled={busy} onClick={()=>setRefresh(n=>n+1)}>Refresh history</button></div></>:!error?<p role="status">Loading publication settings…</p>:null}</section>;
}

interface WebhookStatus {enabled:boolean;secret_configured:boolean;accepted_deliveries:number;last_received_at:number|null}
function WebhookSettings({run}:{run:typeof request}){
  const [status,setStatus]=useState<WebhookStatus|null>(null);
  const [error,setError]=useState('');
  const [busy,setBusy]=useState(false);
  const [refresh,setRefresh]=useState(0);
  useEffect(()=>{
    const controller=new AbortController();
    run<WebhookStatus>('/api/webhooks/github/status',{signal:controller.signal}).then(value=>{if(!controller.signal.aborted){setStatus(value);setError('');}}).catch(e=>{if(!controller.signal.aborted)setError(errorMessage(e));});
    return ()=>controller.abort();
  },[run,refresh]);
  async function configure(enabled:boolean){
    setBusy(true);setError('');
    try{await run('/api/webhooks/github/config',json('PUT',{enabled}));setStatus(null);setRefresh(n=>n+1);}
    catch(e){setError(errorMessage(e));}finally{setBusy(false);}
  }
  return <section className="panel section"><h2>GitHub webhook administration</h2><p>Receive signed delivery receipts at /api/webhooks/github. Automatic synchronization is not enabled. Configure the signing secret on the server, never in the browser.</p>{error&&<p role="alert" className="error">{error}</p>}{status?<><p role="status">Ingestion: {status.enabled?'Enabled':'Disabled'} · Signing secret: {status.secret_configured?'Configured':'Not configured'}</p><p>Unique accepted deliveries: {status.accepted_deliveries}</p><p>Last receipt: {status.last_received_at===null?'None':new Date(status.last_received_at*1000).toLocaleString()}</p><button disabled={busy||(!status.enabled&&!status.secret_configured)} onClick={()=>configure(!status.enabled)}>{status.enabled?'Disable ingestion':'Enable ingestion'}</button></>:!error?<p role="status">Loading webhook configuration…</p>:null}<button disabled={busy} onClick={()=>setRefresh(n=>n+1)}>Refresh webhook status</button></section>;
}
