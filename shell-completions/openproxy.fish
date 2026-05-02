# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_openproxy_global_optspecs
	string join \n host= port= log-filter= data-dir= h/help
end

function __fish_openproxy_needs_command
	# Figure out if the current invocation already has a command.
	set -l cmd (commandline -opc)
	set -e cmd[1]
	argparse -s (__fish_openproxy_global_optspecs) -- $cmd 2>/dev/null
	or return
	if set -q argv[1]
		# Also print the command, so this can be used to figure out what it is.
		echo $argv[1]
		return 1
	end
	return 0
end

function __fish_openproxy_using_subcommand
	set -l cmd (__fish_openproxy_needs_command)
	test -z "$cmd"
	and return 1
	contains -- $cmd[1] $argv
end

complete -c openproxy -n "__fish_openproxy_needs_command" -l host -r
complete -c openproxy -n "__fish_openproxy_needs_command" -l port -r
complete -c openproxy -n "__fish_openproxy_needs_command" -l log-filter -r
complete -c openproxy -n "__fish_openproxy_needs_command" -l data-dir -r -F
complete -c openproxy -n "__fish_openproxy_needs_command" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_needs_command" -f -a "provider"
complete -c openproxy -n "__fish_openproxy_needs_command" -f -a "key"
complete -c openproxy -n "__fish_openproxy_needs_command" -f -a "pool"
complete -c openproxy -n "__fish_openproxy_needs_command" -f -a "completion"
complete -c openproxy -n "__fish_openproxy_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and not __fish_seen_subcommand_from list add help" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and not __fish_seen_subcommand_from list add help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and not __fish_seen_subcommand_from list add help" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and not __fish_seen_subcommand_from list add help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from list" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from add" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from add" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from help" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand provider; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and not __fish_seen_subcommand_from list add help" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and not __fish_seen_subcommand_from list add help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and not __fish_seen_subcommand_from list add help" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and not __fish_seen_subcommand_from list add help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from list" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from add" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from add" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand key; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -f -a "status"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -f -a "create"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -f -a "delete"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and not __fish_seen_subcommand_from list status create delete help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from list" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from status" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from status" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from create" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from create" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from delete" -l json
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from delete" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from help" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from help" -f -a "status"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from help" -f -a "create"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from help" -f -a "delete"
complete -c openproxy -n "__fish_openproxy_using_subcommand pool; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand completion" -s h -l help -d 'Print help'
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and not __fish_seen_subcommand_from provider key pool completion help" -f -a "provider"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and not __fish_seen_subcommand_from provider key pool completion help" -f -a "key"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and not __fish_seen_subcommand_from provider key pool completion help" -f -a "pool"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and not __fish_seen_subcommand_from provider key pool completion help" -f -a "completion"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and not __fish_seen_subcommand_from provider key pool completion help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from provider" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from provider" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from key" -f -a "add"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from pool" -f -a "list"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from pool" -f -a "status"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from pool" -f -a "create"
complete -c openproxy -n "__fish_openproxy_using_subcommand help; and __fish_seen_subcommand_from pool" -f -a "delete"
