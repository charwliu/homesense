// Offline browser codec regression; no network or physical devices.
const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const context=vm.createContext({});
vm.runInContext(fs.readFileSync(require('node:path').join(__dirname,'../crates/homesensed/src/web/draft-json.js'),'utf8'),context);
const encode=context.encodeDraftJson;
const devices='{"revision":9223372036854775807,"name":"literal 9007199254740993","value":9007199254740993}';
const rules='{"configuration_revision":9223372036854775807,"negative":-9007199254740993}';
const output=encode('9007199254740993','9223372036854775807',devices,rules);
assert.equal(output,`{"expected_revision":9007199254740993,"revision":9223372036854775807,"devices":${devices},"automation":${rules}}`);
assert.doesNotThrow(()=>JSON.parse(output));
for(const [old,next] of [['0','2'],['1','1'],['2','1'],['1','9223372036854775808'],['1','1e3'],['1','+2'],['1','02'],['','2']])assert.throws(()=>encode(old,next,'{}','{}'));
for(const doc of ['{','{} , "injected":1','NaN'])assert.throws(()=>encode('1','2',doc,'{}'));
console.log('Draft JSON precision, bounds, malformed input: passed');
