"use strict";
// Preserve numeric tokens in document text. JSON.parse checks syntax only;
// its floating-point result must never be serialized back into configuration.
function encodeDraftJson(expected, revision, devices, automation) {
  const integer = text => {
    if(!/^[1-9][0-9]{0,18}$/.test(text) || BigInt(text)>9223372036854775807n) throw new Error('版本必须为有效的正整数');
    return text;
  };
  expected=integer(expected);revision=integer(revision);
  if(BigInt(revision)<=BigInt(expected))throw new Error('新版本必须大于活动版本');
  JSON.parse(devices);JSON.parse(automation);
  return `{"expected_revision":${expected},"revision":${revision},"devices":${devices},"automation":${automation}}`;
}
