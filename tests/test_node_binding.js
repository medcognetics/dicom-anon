'use strict'

const assert = require('node:assert/strict')
const binding = require('../bindings/node')

const INVALID_DICOM = Buffer.from('not a DICOM Part 10 object')

assert.throws(
  () => binding.validate(INVALID_DICOM),
  (error) => error instanceof Error && error.message.length > 0,
)

const KEY = Buffer.alloc(32, 0x30)
const pixelGateConfiguration = (sopClass) =>
  JSON.stringify({
    policy: 'dicom',
    options: [],
    require_no_burned_in_annotation: true,
    sop_class_allowlist: [sopClass],
  })

assert.throws(
  () => binding.anonymize(INVALID_DICOM, KEY, 'SUBJ-0001', pixelGateConfiguration('not-a-uid')),
  (error) => error instanceof Error && error.message.includes('SOP Class allowlist'),
)
assert.throws(
  () =>
    binding.anonymize(
      INVALID_DICOM,
      KEY,
      'SUBJ-0001',
      pixelGateConfiguration('1.2.840.10008.5.1.4.1.1.1.2'),
    ),
  (error) => error instanceof Error && !error.message.includes('SOP Class allowlist'),
)
