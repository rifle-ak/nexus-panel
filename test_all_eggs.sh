#!/bin/bash

# Test all eggs from parkervcp/eggs repository
# Uses the built-in import command with detailed logging

OUTPUT_DIR="./converted_eggs"
REPORT_FILE="./conversion_report.txt"

# Clean previous run
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

echo "================================================="
echo "Nexus Panel Egg Converter - Scale Test"
echo "Testing against parkervcp/eggs repository"
echo "================================================="
echo ""

# Run the import command and capture output
./target/release/nexus-panel import \
    --input-dir ./test-configs \
    --output-dir "$OUTPUT_DIR" \
    --continue-on-error 2>&1 | tee conversion_output.log

# Parse results from output
TOTAL=$(grep "Found" conversion_output.log | grep -oP '\d+' || echo "0")
SUCCESS=$(grep "Success:" conversion_output.log | grep -oP '\d+' || echo "0")
ERRORS=$(grep "Errors:" conversion_output.log | grep -oP '\d+' || echo "0")

echo ""
echo "================================================="
echo "FINAL RESULTS"
echo "================================================="
echo "Total eggs found: $TOTAL"
echo "Successful conversions: $SUCCESS"
echo "Failed conversions: $ERRORS"
if [ "$TOTAL" -gt 0 ]; then
    SUCCESS_RATE=$(awk "BEGIN {printf \"%.1f\", ($SUCCESS/$TOTAL)*100}")
    echo "Success rate: $SUCCESS_RATE%"
fi
echo ""
echo "Converted configs available in: $OUTPUT_DIR"
echo "Full log saved to: conversion_output.log"

exit 0
