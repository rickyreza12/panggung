import { describe, expect, it, vi, afterEach } from 'vitest';
import { ApiError, canWrite, componentPath, importBody, listPath, request } from './api';
afterEach(()=>vi.unstubAllGlobals());
describe('catalog API',()=>{
  it('encodes qualified identity and search without losing namespace',()=>{
    expect(componentPath('platform.dev','billing')).toBe('/api/components/platform.dev/billing');
    const url=new URL(listPath({q:'a & b',namespace:'platform'},20,21),'http://localhost');
    expect(Object.fromEntries(url.searchParams)).toEqual({q:'a & b',namespace:'platform',offset:'20',limit:'21'});
  });
  it('restricts client write actions to admin/member',()=>{
    expect(canWrite({id:1,username:'a',role:'Viewer'})).toBe(false);
    expect(canWrite({id:1,username:'a',role:'Admin'})).toBe(true);
    expect(canWrite({id:1,username:'a',role:'member'})).toBe(true);
    expect(canWrite({id:1,username:'a',role:'editor'})).toBe(false);
  });
  it('sends cookie credentials and handles denied requests',async()=>{
    const fetch=vi.fn().mockResolvedValue(new Response('denied',{status:403}));
    vi.stubGlobal('fetch',fetch);
    await expect(request('/api/components',{method:'DELETE'})).rejects.toMatchObject({status:403});
    expect(fetch).toHaveBeenCalledWith('/api/components',{method:'DELETE',credentials:'same-origin'});
  });
  it('handles logout without parsing an empty body',async()=>{
    vi.stubGlobal('fetch',vi.fn().mockResolvedValue(new Response(null,{status:204})));
    await expect(request('/api/auth/logout',{method:'POST'})).resolves.toBeUndefined();
  });
  it('returns an authentication error rather than fabricated data',async()=>{
    vi.stubGlobal('fetch',vi.fn().mockResolvedValue(new Response('',{status:401})));
    await expect(request('/api/auth/me')).rejects.toBeInstanceOf(ApiError);
  });
  it('validates descriptor format and byte limits',()=>{
    expect(importBody('yaml','kind: Component').path).toBe('/api/catalog/import/yaml');
    expect(importBody('json','{"kind":"Component"}').init.body).toBe('{"kind":"Component"}');
    expect(()=>importBody('json','{')).toThrow();
    expect(()=>importBody('yaml','é'.repeat(32769))).toThrow('size limit');
  });
});
