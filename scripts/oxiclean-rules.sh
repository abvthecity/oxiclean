# Sourced by the bench scripts: which command runs each rule, and a config
# file that leaves one rule of its command on, so each rule is timed alone.

# Prints the command that runs the rule $1.
command_of() {
  case $1 in
    unused-exports | unused-types | unused-enum-members | unused-namespace-members) echo unused ;;
    dependencies | heavy-dependencies | catalog | duplicate-versions) echo dependencies ;;
    program-files | overlap | references | tsconfig | ambient-types | isolated-declarations | \
      large-literals | type-bloat | complex-types) echo typecheck ;;
    bloat | barrels) echo bloat ;;
    depth | cycles | duplicates) echo "$1" ;;
    *) echo "unknown rule: $1" >&2; return 1 ;;
  esac
}

# Prints the rules the command $1 runs.
rules_of() {
  case $1 in
    unused) echo unused-exports unused-types unused-enum-members unused-namespace-members ;;
    dependencies) echo dependencies heavy-dependencies catalog duplicate-versions ;;
    typecheck)
      echo program-files overlap references tsconfig ambient-types isolated-declarations \
        large-literals type-bloat complex-types
      ;;
    bloat) echo bloat barrels ;;
    depth | cycles | duplicates) echo "$1" ;;
    *) echo "unknown command: $1" >&2; return 1 ;;
  esac
}

# Writes to the file $2 a config that turns off every rule of the command
# of rule $1 but $1, with the defaults for everything else.
only_rule_config() {
  local rule=$1 file=$2 command r
  local -a off=()
  command=$(command_of "$rule")
  for r in $(rules_of "$command"); do
    if [[ $r != "$rule" ]]; then off+=("\"$r\": \"off\""); fi
  done
  local IFS=,
  printf '{ "rules": { %s } }\n' "${off[*]}" >"$file"
}
