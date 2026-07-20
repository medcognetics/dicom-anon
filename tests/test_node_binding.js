'use strict'

const assert = require('node:assert/strict')
const binding = require('../bindings/node')

const INVALID_DICOM = Buffer.from('not a DICOM Part 10 object')

assert.throws(
  () => binding.validate(INVALID_DICOM),
  (error) => error instanceof Error && error.message.length > 0,
)
