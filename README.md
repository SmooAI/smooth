# Alphametics Solver - JavaScript Implementation

## Problem Overview
The alphametics puzzle involves solving cryptarithmetic equations where letters represent unique digits. For example, "SEND + MORE == MONEY" where each letter maps to a unique digit (0-9).

## Solution Approach
Implemented a backtracking algorithm that:
1. Parses the puzzle string to extract terms and result
2. Identifies all unique letters and first letters (which cannot be zero)
3. Uses recursive backtracking to try all valid digit assignments
4. Validates each assignment against the mathematical equation
5. Returns the first valid solution found

## Key Features
- Proper parsing of equations with "==" operator
- Constraint enforcement (first letters ≠ 0)
- Error handling for invalid formats and too many unique letters
- Efficient backtracking algorithm
- Mathematical validation of solutions

## Implementation Details
The solution consists of four main functions:
1. `solve()` - Main entry point that parses and initiates the solving process
2. `solvePuzzle()` - Sets up initial state for backtracking
3. `permuteAndCheck()` - Recursive backtracking function that tries digit assignments
4. `isValidAssignment()` - Validates if a digit assignment satisfies the equation

## Usage
```javascript
import { solve } from './alphametics.js';

// Solve classic puzzle
const solution = solve('SEND + MORE == MONEY');
console.log(solution); // { S: 9, E: 5, N: 6, D: 7, M: 1, O: 0, R: 8, Y: 2 }
```

The implementation is complete and should pass all tests for the alphametics challenge.