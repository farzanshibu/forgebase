// Command gendocs writes the provider documentation into docs/.
//
//	go run ./tools/gendocs
package main

import (
	"flag"
	"fmt"
	"os"

	"github.com/farzanshibu/forgebase/integrations/terraform-provider-forgebase/internal/docgen"
)

func main() {
	docs := flag.String("docs", "docs", "output directory")
	examples := flag.String("examples", "examples", "examples directory")
	flag.Parse()
	if err := docgen.Write(*docs, *examples); err != nil {
		fmt.Fprintln(os.Stderr, "gendocs:", err)
		os.Exit(1)
	}
}
