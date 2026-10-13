import Lake
open Lake DSL

package minicompiler

@[default_target]
lean_exe minic where
  root := `Main
