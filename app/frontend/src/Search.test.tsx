import {describe,it,expect} from 'vitest';
import {searchPath} from './Search';
describe('search request',()=>{it('encodes literal user input and pagination',()=>{const url=new URL(searchPath('a & OR "b"',40),'http://localhost');expect(Object.fromEntries(url.searchParams)).toEqual({q:'a & OR "b"',offset:'40',limit:'20'});});});
