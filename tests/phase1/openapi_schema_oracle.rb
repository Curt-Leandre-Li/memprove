# frozen_string_literal: true

require "digest"
require "fileutils"
require "json"
require "open3"
require "pathname"
require "tmpdir"
require "yaml"

module OpenapiSchemaOracle
  ROOT = Pathname.new(__dir__).join("../..").expand_path.freeze
  OPENAPI_PATH = ROOT.join("openapi/control-api-v1.yaml").freeze
  TOOL_LOCK_PATH = ROOT.join("docs/evaluation/phase-0-gate-tool-lock.json").freeze
  SHA256_PATTERN = /\Asha256:[0-9a-f]{64}\z/
  FILE_OBSERVATION_KEYS = %w[
    path device inode uid gid mode nlink size ctime_ns mtime_ns sha256
  ].freeze

  module_function

  def resolve_json_pointer(document, pointer)
    raise ArgumentError, "not a local JSON pointer: #{pointer}" unless pointer.start_with?("#/")

    pointer.delete_prefix("#/").split("/").inject(document) do |current, token|
      current.fetch(token.gsub("~1", "/").gsub("~0", "~"))
    end
  end

  def transform_openapi_schema(value)
    case value
    when Hash
      value.each_with_object({}) do |(key, child), transformed|
        transformed[key] =
          if key == "$ref" && child.is_a?(String)
            child.sub(%r{\A#/components/schemas/}, "#/$defs/")
          else
            transform_openapi_schema(child)
          end
      end
    when Array
      value.map { |child| transform_openapi_schema(child) }
    else
      value
    end
  end

  def validator_lock
    lock = JSON.parse(TOOL_LOCK_PATH.read)
    tool = lock.fetch("tools").find { |entry| entry.fetch("id") == "sourcemeta-jsonschema" }
    raise "pinned sourcemeta-jsonschema tool is absent from the lock" unless tool
    tool
  end

  def timestamp_ns(time)
    (time.to_i * 1_000_000_000) + time.nsec
  end

  def reject_symlink_components!(path, label)
    expanded = Pathname.new(path).expand_path
    current = Pathname.new(File::SEPARATOR)
    expanded.each_filename do |component|
      current = current.join(component)
      raise ArgumentError, "#{label} contains a symlink: #{current}" if current.lstat.symlink?
    end
    expanded
  rescue Errno::ENOENT, Errno::ELOOP, Errno::ENOTDIR => error
    raise ArgumentError, "#{label} is unavailable: #{error.message}"
  end

  def capture_file_observation!(path)
    candidate = reject_symlink_components!(path, "validator path")
    stat = candidate.lstat
    unless stat.file? && stat.nlink == 1 && candidate.realpath == candidate
      raise ArgumentError, "validator path is not a canonical single-link regular file"
    end
    {
      "path" => candidate.to_s,
      "device" => stat.dev,
      "inode" => stat.ino,
      "uid" => stat.uid,
      "gid" => stat.gid,
      "mode" => format("%04o", stat.mode & 0o7777),
      "nlink" => stat.nlink,
      "size" => stat.size,
      "ctime_ns" => timestamp_ns(stat.ctime),
      "mtime_ns" => timestamp_ns(stat.mtime),
      "sha256" => "sha256:#{Digest::SHA256.file(candidate).hexdigest}"
    }
  end

  def validate_validator_binding!(binding)
    unless binding.is_a?(Hash)
      raise ArgumentError, "staged validator binding is required"
    end
    expected_keys = %w[
      observation sha256 source_path stage_id stage_root staged_path
    ]
    unless binding.keys.sort == expected_keys
      raise ArgumentError, "staged validator binding has an invalid field set"
    end

    root = reject_symlink_components!(binding.fetch("stage_root"), "validator stage root")
    path = reject_symlink_components!(binding.fetch("staged_path"), "staged validator path")
    source = Pathname.new(binding.fetch("source_path")).expand_path
    unless root.absolute? && root.directory? && root.realpath == root
      raise ArgumentError, "validator stage root is not a canonical directory"
    end
    unless path.absolute? && path.file? && path.executable? && path.realpath == path &&
           path.to_s.start_with?("#{root}#{File::SEPARATOR}")
      raise ArgumentError, "staged validator path must be inside the stage"
    end
    if source == path || source.to_s.start_with?("#{root}#{File::SEPARATOR}")
      raise ArgumentError, "validator source path cannot be used as the staged executable"
    end
    digest = binding.fetch("sha256")
    unless digest.is_a?(String) && digest.match?(SHA256_PATTERN)
      raise ArgumentError, "staged validator binding digest is invalid"
    end
    observation = binding.fetch("observation")
    unless observation.is_a?(Hash) && observation.keys.sort == FILE_OBSERVATION_KEYS.sort
      raise ArgumentError, "staged validator observation has an invalid field set"
    end
    actual = capture_file_observation!(path)
    unless actual == observation && actual.fetch("sha256") == digest
      raise ArgumentError, "staged validator observation does not match the bound object"
    end

    yield(path, root, actual) if block_given?
    [path, root, actual]
  end

  def write_schema(temp_root, openapi, pointer)
    output = temp_root.join("openapi/schema-#{Digest::SHA256.hexdigest(pointer)[0, 16]}.json")
    return output if output.file?

    selected = resolve_json_pointer(openapi, pointer)
    wrapper = {
      "$schema" => openapi.fetch("jsonSchemaDialect"),
      "$defs" => transform_openapi_schema(openapi.dig("components", "schemas")),
      "allOf" => [transform_openapi_schema(selected)]
    }
    output.write(JSON.pretty_generate(wrapper))
    output
  end

  def execute_staged_validator!(validator_path, stage_root, bound_observation, argv,
                                temp_root, execution_log, invocation_kind)
    pre = capture_file_observation!(validator_path)
    unless pre == bound_observation
      raise "staged validator identity changed before #{invocation_kind} invocation"
    end
    unless argv.first == validator_path.to_s &&
           argv.first.start_with?("#{stage_root}#{File::SEPARATOR}")
      raise "staged validator argv escaped the stage"
    end
    environment = {
      "HOME" => Dir.home,
      "LC_ALL" => "C.UTF-8",
      "PATH" => "/usr/bin:/bin:/usr/sbin:/sbin",
      "TMPDIR" => temp_root.to_s
    }
    stdout, stderr, status = Open3.capture3(
      environment,
      *argv,
      chdir: temp_root.to_s,
      unsetenv_others: true
    )
    post = capture_file_observation!(validator_path)
    execution_log << {
      "sequence" => execution_log.length + 1,
      "kind" => invocation_kind,
      "declared_argv" => argv,
      "actual_argv" => argv,
      "pre_object" => pre,
      "post_object" => post,
      "exit_code" => status.exitstatus,
      "pre_post_equal" => pre == post
    }
    raise "staged validator identity changed after #{invocation_kind} invocation" unless pre == post

    [stdout, stderr, status]
  end

  def validate_case(validator_path, stage_root, bound_observation, temp_root, openapi, record,
                    execution_log)
    case_id = record.fetch("case_id")
    schema_path = write_schema(temp_root, openapi, record.fetch("schema_pointer"))
    instance_path = temp_root.join("instance-#{Digest::SHA256.hexdigest(case_id)[0, 16]}.json")
    instance_path.binwrite(record.fetch("payload_raw"))

    argv = [
      validator_path.to_s,
      "validate",
      schema_path.to_s,
      instance_path.to_s,
      "--resolve",
      temp_root.join("schemas").to_s,
      "--format-assertion",
      "--json"
    ]
    stdout, stderr, status = execute_staged_validator!(
      validator_path,
      stage_root,
      bound_observation,
      argv,
      temp_root,
      execution_log,
      "validation"
    )
    unless [0, 2].include?(status.exitstatus)
      raise "#{case_id}: validator execution failed with #{status.exitstatus}: #{stderr}"
    end

    result = JSON.parse(stdout)
    valid = result.fetch("valid")
    expected_exit = valid ? 0 : 2
    raise "#{case_id}: validator result/exit mismatch" unless status.exitstatus == expected_exit

    { "case_id" => case_id, "valid" => valid }
  end

  def run(fixture_path, validator_binding: nil)
    fixture = JSON.parse(Pathname.new(fixture_path).binread)
    cases = fixture.fetch("cases")
    ids = cases.map { |record| record.fetch("case_id") }
    raise "duplicate parity case_id" unless ids.length == ids.uniq.length

    openapi = YAML.load_file(OPENAPI_PATH.to_s)
    tool = validator_lock
    validator_path, stage_root, bound_observation =
      validate_validator_binding!(validator_binding)
    digest = bound_observation.fetch("sha256").delete_prefix("sha256:")
    execution_log = []
    results = Dir.mktmpdir("continuity-openapi-wire-parity-", "/private/tmp") do |directory|
      temp_root = Pathname.new(directory)
      FileUtils.cp_r(ROOT.join("schemas"), temp_root.join("schemas"))
      FileUtils.mkdir_p(temp_root.join("openapi"))
      version_stdout, version_stderr, version_status = execute_staged_validator!(
        validator_path,
        stage_root,
        bound_observation,
        [validator_path.to_s, "version"],
        temp_root,
        execution_log,
        "version"
      )
      unless version_status.success?
        raise "validator version command failed: #{version_stderr}"
      end
      actual_version = version_stdout.strip
      unless actual_version == tool.fetch("version")
        raise "validator version mismatch: expected #{tool.fetch('version')}, got #{actual_version}"
      end
      cases.map do |record|
        validate_case(
          validator_path,
          stage_root,
          bound_observation,
          temp_root,
          openapi,
          record,
          execution_log
        )
      end
    end
    version_count = execution_log.count { |entry| entry.fetch("kind") == "version" }
    validation_count = execution_log.count { |entry| entry.fetch("kind") == "validation" }

    {
      "schema_version" => 1,
      "openapi_sha256" => Digest::SHA256.file(OPENAPI_PATH).hexdigest,
      "validator" => {
        "id" => tool.fetch("id"),
        "version" => tool.fetch("version"),
        "sha256" => digest,
        "path" => validator_path.to_s,
        "format_assertion" => true,
        "network_enabled" => false
      },
      "staged_execution" => {
        "version_invocations" => version_count,
        "validation_invocations" => validation_count,
        "total_invocations" => execution_log.length,
        "execution_log_sha256" =>
          "sha256:#{Digest::SHA256.hexdigest(JSON.generate(execution_log))}",
        "all_actual_argv_in_stage" => execution_log.all? do |entry|
          entry.fetch("actual_argv").first.start_with?("#{stage_root}#{File::SEPARATOR}")
        end,
        "all_pre_post_observations_equal" => execution_log.all? do |entry|
          entry.fetch("pre_post_equal")
        end
      },
      "cases" => results
    }
  end
end

if $PROGRAM_NAME == __FILE__
  fixture_path = ARGV.fetch(0) do
    warn "usage: ruby tests/phase1/openapi_schema_oracle.rb <fixture.json>"
    exit 64
  end
  binding_json = ENV.fetch("CONTINUITY_SOURCEMETA_BINDING") do
    warn "CONTINUITY_SOURCEMETA_BINDING is required"
    exit 64
  end
  binding = JSON.parse(binding_json)
  executable = ENV["CONTINUITY_SOURCEMETA_EXECUTABLE"]
  unless executable == binding["staged_path"]
    warn "CONTINUITY_SOURCEMETA_EXECUTABLE does not match the staged binding"
    exit 64
  end
  puts JSON.generate(OpenapiSchemaOracle.run(fixture_path, validator_binding: binding))
end
