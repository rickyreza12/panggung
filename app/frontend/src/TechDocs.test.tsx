import { describe,it,expect } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { SafeMarkdown,validateMarkdown,versionsPath } from './TechDocs';

describe('TechDocs safety',()=>{
  it('renders Markdown but never active links, images or raw HTML',()=>{
    const source='# Heading\n\n**Bold** [safe](https://example.com) [bad](javascript:alert%281%29) ![remote](https://example.com/pixel.png)\n\n<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n<div onclick="alert(1)">html</div>';
    const html=renderToStaticMarkup(<SafeMarkdown source={source}/>);
    expect(html).toContain('<h1>Heading</h1>');
    expect(html).toContain('<strong>Bold</strong>');
    expect(html).toContain('safe');
    for(const value of ['<a ','href=','<img','<script','onclick=','onerror=','example.com'])expect(html).not.toContain(value);
  });
  it('checks UTF-8 size and control characters',()=>{
    expect(()=>validateMarkdown('# Valid\n')).not.toThrow();
    expect(()=>validateMarkdown('é'.repeat(32769))).toThrow();
    expect(()=>validateMarkdown('   ')).toThrow();
    expect(()=>validateMarkdown('bad\0')).toThrow();
    expect(versionsPath('platform','billing')).toBe('/api/techdocs/platform/billing/versions');
  });
});
