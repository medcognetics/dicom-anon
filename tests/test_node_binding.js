'use strict'

const assert = require('node:assert/strict')
const binding = require('../bindings/node')

const INVALID_DICOM = Buffer.from('not a DICOM Part 10 object')

assert.throws(
  () => binding.validate(INVALID_DICOM),
  (error) => error instanceof Error && error.message.length > 0,
)

const KEY = Buffer.from('0123456789abcdef0123456789abcdef')
const SECONDARY_CAPTURE = '1.2.840.10008.5.1.4.1.1.7'
const SENTINEL_UID = '2.25.271828182845904523536028747135266249'
const SOURCE_UIDS = [
  SENTINEL_UID,
  '2.25.82751374713386041856794579278177576159',
  '2.25.55250158356079243228430942758422479484',
]
const SAFE_HARBOR = JSON.stringify({
  policy: 'safe-harbor-metadata',
  options: [
    'clean-graphics',
    'clean-structured-content',
    'clean-descriptors',
    'retain-modified-dates',
    'retain-patient-characteristics',
  ],
})

function element(group, number, vr, value) {
  let bytes = Buffer.isBuffer(value) ? value : Buffer.from(value, 'latin1')
  if (bytes.length % 2) {
    bytes = Buffer.concat([bytes, Buffer.from(vr === 'UI' ? '\0' : ' ')])
  }
  const long = vr === 'OB'
  const header = Buffer.alloc(long ? 12 : 8)
  header.writeUInt16LE(group, 0)
  header.writeUInt16LE(number, 2)
  header.write(vr, 4, 'latin1')
  if (long) {
    header.writeUInt32LE(bytes.length, 8)
  } else {
    header.writeUInt16LE(bytes.length, 6)
  }
  return Buffer.concat([header, bytes])
}

function syntheticDicom() {
  const meta = Buffer.concat([
    element(0x0002, 0x0001, 'OB', Buffer.from([0, 1])),
    element(0x0002, 0x0002, 'UI', SECONDARY_CAPTURE),
    element(0x0002, 0x0003, 'UI', SOURCE_UIDS[0]),
    element(0x0002, 0x0010, 'UI', '1.2.840.10008.1.2.1'),
    element(0x0002, 0x0012, 'UI', '2.25.1'),
  ])
  const length = Buffer.alloc(4)
  length.writeUInt32LE(meta.length)
  return Buffer.concat([
    Buffer.alloc(128),
    Buffer.from('DICM'),
    element(0x0002, 0x0000, 'UL', length),
    meta,
    element(0x0008, 0x0016, 'UI', SECONDARY_CAPTURE),
    element(0x0008, 0x0018, 'UI', SOURCE_UIDS[0]),
    element(0x0010, 0x0020, 'LO', 'SYNTHETIC'),
    element(0x0020, 0x000d, 'UI', SOURCE_UIDS[1]),
    element(0x0020, 0x000e, 'UI', SOURCE_UIDS[2]),
  ])
}

const required = JSON.parse(
  binding.requiredUids(syntheticDicom(), KEY, 'SUBJ-0001', SAFE_HARBOR),
)
assert.deepEqual(
  required.map((entry) => entry.path),
  ['(0008,0018)', '(0020,000D)', '(0020,000E)'],
)
assert.deepEqual(new Set(required.map((entry) => entry.uid)), new Set(SOURCE_UIDS))

const first = binding.anonymizeWithGeneratedUidMap(syntheticDicom(), KEY, 'SUBJ-0001', SAFE_HARBOR)
const second = binding.anonymizeWithGeneratedUidMap(syntheticDicom(), KEY, 'SUBJ-0001', SAFE_HARBOR)
const firstMap = JSON.parse(first.uidMapJson)
const secondMap = JSON.parse(second.uidMapJson)
assert.deepEqual(new Set(Object.keys(firstMap)), new Set(SOURCE_UIDS))
for (const uid of SOURCE_UIDS) {
  assert.notEqual(firstMap[uid], secondMap[uid])
  assert.equal(first.data.includes(uid), false)
  assert.equal(first.reportJson.includes(uid), false)
}

assert.throws(
  () => binding.anonymize(syntheticDicom(), KEY, 'SUBJ-0001', SAFE_HARBOR),
  (error) => error.message.includes('(0008,0018)') && !error.message.includes(SENTINEL_UID),
)
