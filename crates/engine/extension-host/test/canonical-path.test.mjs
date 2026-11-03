import test from 'node:test'
import assert from 'node:assert/strict'
import {insideKey,pathKey,samePath,stripVerbatim,unlinkedKeyMatches} from '../dist/shell-hooks.mjs'

// Pure string normalization: runs on every OS by naming the platform.
test('win32 canonical compare ignores the verbatim prefix and case, never the path itself',()=>{
  assert.equal(stripVerbatim('\\\\?\\C:\\Users\\runneradmin\\x','win32'),'C:\\Users\\runneradmin\\x')
  assert.equal(stripVerbatim('\\\\?\\UNC\\host\\share\\x','win32'),'\\\\host\\share\\x')
  assert.ok(samePath('\\\\?\\C:\\Users\\RunnerAdmin\\Bundle\\hooks.json','c:\\users\\runneradmin\\bundle\\hooks.json','win32'))
  assert.equal(pathKey('\\\\?\\C:\\A\\B','win32'),pathKey('c:\\a\\b','win32'))
  assert.ok(!samePath('C:\\a\\b','C:\\a\\c','win32'))
  // Case folds only on win32.
  assert.ok(!samePath('/a/B','/a/b','linux'))
  // Containment against a differently spelled canonical root keeps the target's case.
  assert.equal(insideKey('\\\\?\\C:\\Users\\RUNNERADMIN\\bundle','c:\\users\\runneradmin\\bundle\\Mod\\a.js','win32'),'Mod/a.js')
  assert.equal(insideKey('C:\\bundle','C:\\bundle-evil\\a.js','win32'),undefined)
  assert.equal(insideKey('C:\\bundle','D:\\bundle\\a.js','win32'),undefined)
  assert.equal(insideKey('/r','/r/../x','linux'),undefined)
  // A file is unlinked when its canonical path is the canonical root joined with its reviewed key.
  assert.ok(unlinkedKeyMatches('\\\\?\\C:\\Users\\runneradmin\\bundle','hooks.json','C:\\USERS\\RunnerAdmin\\Bundle\\HOOKS.JSON','win32'))
  assert.ok(!unlinkedKeyMatches('C:\\bundle','link.json','C:\\bundle\\hooks.json','win32'))
  assert.ok(!unlinkedKeyMatches('C:\\bundle','../x.json','C:\\x.json','win32'))
  assert.ok(!unlinkedKeyMatches('/r','link.json','/r/hooks.json','linux'))
})
