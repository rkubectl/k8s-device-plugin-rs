// Optional independent fixture generator; not required by Rust builds or CI.
package main

import (
	"encoding/json"
	"fmt"
	"os"

	"k8s.io/apimachinery/pkg/api/resource"
)

type row struct {
	Input string `json:"input"`
	Wire  string `json:"wire,omitempty"`
	Valid bool   `json:"valid"`
}

func main() {
	var inputs []string
	if err := json.NewDecoder(os.Stdin).Decode(&inputs); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	rows := make([]row, 0, len(inputs))
	for _, input := range inputs {
		quantity, err := resource.ParseQuantity(input)
		result := row{Input: input, Valid: err == nil}
		if err == nil {
			result.Wire = quantity.String()
		}
		rows = append(rows, result)
	}
	if err := json.NewEncoder(os.Stdout).Encode(rows); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
